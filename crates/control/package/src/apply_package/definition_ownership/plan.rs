use super::{
    ApplyPackageError, ApplyPackageErrorType, BTreeSet,
    DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL, DefinitionAction, DefinitionType,
    HISTORY_TABLE_SUFFIX, MigrationPolicyError, MigrationPolicyErrorType, ModelDeclaration,
    PackageDirectory, PackageManifest, Path, PlannedDefinitionMutation, ensure,
    inspect_migration_definition_mutations, is_history_table_name,
};
use anyhow::Context as _;

pub(in super::super) fn model_for_relation<'a>(
    manifest: &'a PackageManifest,
    schema: &str,
    relation: &str,
) -> Option<&'a ModelDeclaration> {
    manifest
        .models
        .values()
        .find(|model| model.schema == schema && model.table == relation)
}

pub(in super::super) fn definition_error(
    type_: ApplyPackageErrorType,
    coordinate: &str,
    planned: &PlannedDefinitionMutation,
    owner_package: Option<&str>,
    detail: impl Into<String>,
) -> ApplyPackageError {
    definition_error_for_parts(
        type_,
        coordinate,
        planned,
        planned.mutation.definition_type(),
        planned.mutation.definition(),
        owner_package,
        detail,
    )
}

pub(super) fn definition_error_for_parts(
    type_: ApplyPackageErrorType,
    coordinate: &str,
    planned: &PlannedDefinitionMutation,
    definition_type: DefinitionType,
    definition: &str,
    owner_package: Option<&str>,
    detail: impl Into<String>,
) -> ApplyPackageError {
    ApplyPackageError {
        type_,
        coordinate: coordinate.to_owned(),
        path: Some(planned.relative_path.to_string()),
        schema: Some(planned.mutation.schema().to_owned()),
        relation: Some(planned.mutation.relation().to_owned()),
        definition_type: Some(definition_type),
        definition: Some(definition.to_owned()),
        owner_package: owner_package.map(str::to_owned),
        detail: detail.into(),
        source: None,
    }
}

pub(in super::super) fn validate_migration_policy(
    package_root: &Path,
    directory: &PackageDirectory,
    plan: &wamn_schema_control::PackageMigrationPlan,
) -> anyhow::Result<MigrationPolicyPlan> {
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes)?;
    validate_manifest_definition_owners(&manifest)?;
    // A package with no SQL owns no relation and ships no migration.
    if plan.models.is_empty()
        && plan.cdc_excluded_relations.is_empty()
        && directory.migrations.is_empty()
    {
        return Ok(MigrationPolicyPlan {
            mutations: Vec::new(),
            deferred: None,
        });
    }
    ensure!(
        !plan.models.is_empty(),
        "package-migration-schema-missing: manifest must map at least one model to an application schema"
    );
    let mut schemas = plan
        .models
        .iter()
        .map(|model| model.schema.as_str())
        .chain(
            plan.cdc_excluded_relations
                .iter()
                .map(|relation| relation.schema.as_str()),
        )
        .collect::<Vec<_>>();
    schemas.sort_unstable();
    schemas.dedup();
    let mut mutations = Vec::new();
    let mut deferred = None;
    for migration in &directory.migrations {
        let path = package_root.join(&migration.relative_path);
        let inspected =
            inspect_migration_definition_mutations(&path, &migration.bytes, &schemas)
                .with_context(|| format!("inspect {} before apply", migration.relative_path))?;
        let earlier = mutations
            .iter()
            .map(|planned: &PlannedDefinitionMutation| &planned.mutation)
            .collect::<Vec<_>>();
        let validation =
            wamn_schema_introspection::migration_policy::validate_migration_bytes_for_schemas(
                &path,
                &migration.bytes,
                &schemas,
                &earlier,
            );
        match validation {
            Ok(()) => {}
            Err(source)
                if source.error_type() == MigrationPolicyErrorType::UnsupportedStatement
                    && inspected.iter().any(|mutation| {
                        matches!(
                            mutation.action(),
                            DefinitionAction::Alter | DefinitionAction::Drop
                        )
                    }) =>
            {
                if deferred.is_none() {
                    deferred = Some(DeferredMigrationPolicyError {
                        relative_path: migration.relative_path.clone().into_boxed_str(),
                        source,
                    });
                }
            }
            Err(source) => {
                return Err(source)
                    .with_context(|| format!("validate {} before apply", migration.relative_path));
            }
        }
        pg_query::parse(std::str::from_utf8(&migration.bytes)?)
            .with_context(|| format!("parse {} before apply", migration.relative_path))?;
        mutations.extend(
            inspected
                .into_iter()
                .map(|mutation| PlannedDefinitionMutation {
                    relative_path: migration.relative_path.clone().into_boxed_str(),
                    mutation,
                }),
        );
    }
    validate_relation_classifications(plan, &mutations)?;
    let coordinate = format!(
        "{}@{}",
        plan.coordinate.package_id(),
        plan.coordinate.package_version()
    );
    for planned in &mutations {
        let mutation = &planned.mutation;
        let Some(model) = model_for_relation(&manifest, mutation.schema(), mutation.relation())
        else {
            continue;
        };
        let owner =
            explicit_definition_owner(model, mutation.definition_type(), mutation.definition());
        if matches!(
            mutation.action(),
            DefinitionAction::Create | DefinitionAction::Add
        ) && (owner.is_some_and(|owner| owner != manifest.package.id)
            || (mutation.action() == DefinitionAction::Add
                && model.owner != manifest.package.id
                && owner != Some(manifest.package.id.as_str())))
        {
            return Err(definition_error(
                ApplyPackageErrorType::DefinitionOwnerDeclarationMissing,
                &coordinate,
                planned,
                owner,
                "the manifest must name the applying package as owner of each created or added definition",
            )
            .into());
        }
    }
    Ok(MigrationPolicyPlan {
        mutations,
        deferred,
    })
}

pub(super) fn validate_relation_classifications(
    plan: &wamn_schema_control::PackageMigrationPlan,
    mutations: &[PlannedDefinitionMutation],
) -> anyhow::Result<()> {
    let created = mutations
        .iter()
        .filter(|planned| planned.mutation.action() == DefinitionAction::Create)
        .map(|planned| {
            (
                planned.mutation.schema(),
                planned.mutation.relation(),
                planned.relative_path.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    for (schema, relation, path) in &created {
        ensure!(
            !is_history_table_name(relation),
            "history-table-name-reserved: {path} creates {schema}.{relation}, but the {HISTORY_TABLE_SUFFIX} suffix is reserved for the history tables that apply-package creates"
        );
        let modeled = plan
            .models
            .iter()
            .any(|model| model.schema == *schema && model.table == *relation);
        let excluded = plan
            .cdc_excluded_relations
            .iter()
            .any(|item| item.schema == *schema && item.table == *relation);
        ensure!(
            modeled || excluded,
            "{DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL}: {path} creates {schema}.{relation}, but wamn.json declares it as neither a model nor an internal relation with cdc excluded"
        );
    }
    // apply-package creates each history table, so no migration creates one.
    for excluded in plan
        .cdc_excluded_relations
        .iter()
        .filter(|excluded| !is_history_table_name(&excluded.table))
    {
        ensure!(
            created.iter().any(|(schema, relation, _)| {
                *schema == excluded.schema && *relation == excluded.table
            }),
            "cdc-excluded-relation-missing: {} names {}.{}, but the package migration stream does not create it",
            excluded.relation_id,
            excluded.schema,
            excluded.table
        );
    }
    Ok(())
}

fn validate_manifest_definition_owners(manifest: &PackageManifest) -> anyhow::Result<()> {
    let admitted = std::iter::once(manifest.package.id.as_str())
        .chain(
            manifest
                .base_dependencies
                .values()
                .map(|dependency| dependency.package.as_str()),
        )
        .collect::<BTreeSet<_>>();
    for (model_id, model) in &manifest.models {
        for (definition, owner) in std::iter::once(("relation", model.owner.as_str()))
            .chain(
                model
                    .field_owners
                    .iter()
                    .map(|(field, owner)| (field.as_str(), owner.as_str())),
            )
            .chain(
                model
                    .constraint_owners
                    .iter()
                    .map(|(constraint, owner)| (constraint.as_str(), owner.as_str())),
            )
        {
            ensure!(
                admitted.contains(owner),
                "definition-owner-undeclared: {model_id}.{definition} names {owner}, which is neither the package nor a declared base"
            );
        }
    }
    Ok(())
}

pub(super) fn explicit_definition_owner<'a>(
    model: &'a ModelDeclaration,
    type_: DefinitionType,
    definition: &str,
) -> Option<&'a str> {
    match type_ {
        DefinitionType::Relation => Some(model.owner.as_str()),
        DefinitionType::Field => model.field_owners.get(definition).map(String::as_str),
        DefinitionType::Constraint => model.constraint_owners.get(definition).map(String::as_str),
    }
}

#[derive(Debug)]
pub(in super::super) struct DeferredMigrationPolicyError {
    pub(in super::super) relative_path: Box<str>,
    pub(in super::super) source: MigrationPolicyError,
}

#[derive(Debug)]
pub(in super::super) struct MigrationPolicyPlan {
    pub(in super::super) mutations: Vec<PlannedDefinitionMutation>,
    pub(in super::super) deferred: Option<DeferredMigrationPolicyError>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_owners_must_name_the_package_or_a_declared_base() {
        let mut manifest = PackageManifest::from_slice(include_bytes!(
            "../../../tests/fixtures/observer_package/wamn.json"
        ))
        .unwrap();
        let model = manifest.models.get_mut("item").unwrap();
        model
            .field_owners
            .insert("note".into(), "source_fixture".into());
        model
            .constraint_owners
            .insert("item_note_key".into(), "source_fixture".into());
        validate_manifest_definition_owners(&manifest).unwrap();
        for definition in ["relation", "field", "constraint"] {
            let mut manifest = manifest.clone();
            let model = manifest.models.get_mut("item").unwrap();
            match definition {
                "relation" => model.owner = "undeclared".into(),
                "field" => {
                    model
                        .field_owners
                        .insert("note".into(), "undeclared".into());
                }
                "constraint" => {
                    model
                        .constraint_owners
                        .insert("item_note_key".into(), "undeclared".into());
                }
                _ => unreachable!(),
            }
            let error = validate_manifest_definition_owners(&manifest).unwrap_err();
            assert!(
                error
                    .to_string()
                    .starts_with("definition-owner-undeclared:"),
                "{error}"
            );
        }
    }
}

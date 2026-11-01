use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
use wamn_record_history::{HISTORY_TABLE_SUFFIX, is_history_table_name};
use wamn_schema_control::PackageDirectory;
use wamn_schema_generator::{ModelDeclaration, PackageManifest};
use wamn_schema_introspection::migration_policy::{
    DefinitionAction, DefinitionMutation, DefinitionType, MigrationPolicyError,
    MigrationPolicyErrorType, inspect_migration_definition_mutations,
};

use super::error::{
    ApplyPackageError, ApplyPackageErrorType, DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL,
};

const SELECT_DEFINITION_OWNER_SQL: &str = "\
SELECT owner_package_id, client_field_extensible \
  FROM catalog.package_definition_owners \
 WHERE env_key = $1 AND schema_name = $2 AND relation_name = $3 \
   AND definition_type = $4 AND definition_name = $5";
const INSERT_DEFINITION_OWNER_SQL: &str = "\
INSERT INTO catalog.package_definition_owners \
    (env_key, schema_name, relation_name, definition_type, definition_name, \
     owner_package_id, client_field_extensible) \
VALUES ($1, $2, $3, $4, $5, $6, $7) \
ON CONFLICT (env_key, schema_name, relation_name, definition_type, definition_name) \
DO NOTHING";
pub(super) const SELECT_RELATION_PRESENT_SQL: &str = "\
SELECT EXISTS (\
    SELECT 1 FROM pg_catalog.pg_class AS relation \
    JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
    WHERE namespace.nspname = $1 AND relation.relname = $2 AND relation.relkind = 'r'\
)";
const SELECT_FIELD_PRESENT_SQL: &str = "\
SELECT EXISTS (\
    SELECT 1 FROM pg_catalog.pg_attribute AS field \
    JOIN pg_catalog.pg_class AS relation ON relation.oid = field.attrelid \
    JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
    WHERE namespace.nspname = $1 AND relation.relname = $2 \
      AND relation.relkind = 'r' AND field.attname = $3 \
      AND field.attnum > 0 AND NOT field.attisdropped\
)";
const SELECT_CONSTRAINT_PRESENT_SQL: &str = "\
SELECT EXISTS (\
    SELECT 1 FROM pg_catalog.pg_constraint AS definition \
    JOIN pg_catalog.pg_class AS relation ON relation.oid = definition.conrelid \
    JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
    WHERE namespace.nspname = $1 AND relation.relname = $2 \
      AND relation.relkind = 'r' AND definition.conname = $3 \
      AND definition.contype IN ('p', 'u', 'f', 'c')\
)";
const SELECT_RELATION_DEFINITIONS_SQL: &str = "\
SELECT definition_type, definition_name FROM (\
    SELECT 'field'::text AS definition_type, field.attname::text AS definition_name, \
           field.attnum::int AS ordering \
      FROM pg_catalog.pg_attribute AS field \
      JOIN pg_catalog.pg_class AS relation ON relation.oid = field.attrelid \
      JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
     WHERE namespace.nspname = $1 AND relation.relname = $2 \
       AND relation.relkind = 'r' AND field.attnum > 0 AND NOT field.attisdropped \
    UNION ALL \
    SELECT 'constraint'::text, definition.conname::text, 1000000 \
      FROM pg_catalog.pg_constraint AS definition \
      JOIN pg_catalog.pg_class AS relation ON relation.oid = definition.conrelid \
      JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
     WHERE namespace.nspname = $1 AND relation.relname = $2 \
       AND relation.relkind = 'r' AND definition.contype IN ('p', 'u', 'f', 'c')\
) AS definitions ORDER BY ordering, definition_type, definition_name COLLATE \"C\"";

#[derive(Debug)]
pub(super) struct PlannedDefinitionMutation {
    pub(super) relative_path: Box<str>,
    pub(super) mutation: DefinitionMutation,
}

#[derive(Debug)]
pub(super) struct StoredDefinitionOwner {
    pub(super) package_id: String,
    pub(super) client_field_extensible: bool,
}

pub(super) async fn reconcile_definition_ownership(
    tx: &Transaction<'_>,
    env_key: &str,
    coordinate: &str,
    package_id: &str,
    manifest: &PackageManifest,
    mutations: &[&PlannedDefinitionMutation],
) -> anyhow::Result<bool> {
    let mut changed = false;
    for planned in mutations {
        let mutation = &planned.mutation;
        match mutation.action() {
            DefinitionAction::Create => {
                ensure_definition_present(tx, coordinate, planned).await?;
                let extensible =
                    model_for_relation(manifest, mutation.schema(), mutation.relation())
                        .filter(|model| model.owner == package_id)
                        .is_some_and(|model| model.client_field_extensible);
                changed |= insert_definition_owner(
                    tx,
                    env_key,
                    coordinate,
                    planned,
                    DefinitionType::Relation,
                    mutation.relation(),
                    package_id,
                    extensible,
                )
                .await?;
                for row in tx
                    .query(
                        SELECT_RELATION_DEFINITIONS_SQL,
                        &[&mutation.schema(), &mutation.relation()],
                    )
                    .await
                    .context("read server-derived relation definitions")?
                {
                    let kind = match row.get::<_, String>(0).as_str() {
                        "field" => DefinitionType::Field,
                        "constraint" => DefinitionType::Constraint,
                        value => unreachable!("closed server definition kind {value}"),
                    };
                    let definition = row.get::<_, String>(1);
                    changed |= insert_definition_owner(
                        tx,
                        env_key,
                        coordinate,
                        planned,
                        kind,
                        &definition,
                        package_id,
                        false,
                    )
                    .await?;
                }
            }
            DefinitionAction::Add => {
                ensure_definition_present(tx, coordinate, planned).await?;
                changed |= insert_definition_owner(
                    tx,
                    env_key,
                    coordinate,
                    planned,
                    mutation.definition_type(),
                    mutation.definition(),
                    package_id,
                    false,
                )
                .await?;
            }
            DefinitionAction::Alter | DefinitionAction::Drop => {
                unreachable!("migration policy refuses non-additive DDL before execution")
            }
        }
    }
    Ok(changed)
}

async fn ensure_definition_present(
    tx: &Transaction<'_>,
    coordinate: &str,
    planned: &PlannedDefinitionMutation,
) -> anyhow::Result<()> {
    let mutation = &planned.mutation;
    if definition_present(
        tx,
        mutation.schema(),
        mutation.relation(),
        mutation.definition_type(),
        mutation.definition(),
    )
    .await?
    {
        Ok(())
    } else {
        Err(definition_error(
            ApplyPackageErrorType::DefinitionNotFound,
            coordinate,
            planned,
            None,
            "PostgreSQL did not expose the definition after its migration statement",
        )
        .into())
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the parameters are the exact durable definition-owner row"
)]
async fn insert_definition_owner(
    tx: &Transaction<'_>,
    env_key: &str,
    coordinate: &str,
    planned: &PlannedDefinitionMutation,
    kind: DefinitionType,
    definition: &str,
    package_id: &str,
    client_field_extensible: bool,
) -> anyhow::Result<bool> {
    let mutation = &planned.mutation;
    let inserted = tx
        .execute(
            INSERT_DEFINITION_OWNER_SQL,
            &[
                &env_key,
                &mutation.schema(),
                &mutation.relation(),
                &kind.as_str(),
                &definition,
                &package_id,
                &client_field_extensible,
            ],
        )
        .await
        .context("record server-derived definition owner")?;
    if inserted == 1 {
        return Ok(true);
    }
    let existing = load_definition_owner(
        tx,
        env_key,
        mutation.schema(),
        mutation.relation(),
        kind,
        definition,
    )
    .await?
    .expect("the conflicting definition owner row exists");
    if existing.package_id == package_id
        && existing.client_field_extensible == client_field_extensible
    {
        Ok(false)
    } else {
        Err(definition_error_for_parts(
            ApplyPackageErrorType::DefinitionOwnerConflict,
            coordinate,
            planned,
            kind,
            definition,
            Some(existing.package_id.as_str()),
            "the durable owner fact disagrees with the applying package",
        )
        .into())
    }
}

pub(super) async fn load_definition_owner(
    tx: &Transaction<'_>,
    env_key: &str,
    schema: &str,
    relation: &str,
    kind: DefinitionType,
    definition: &str,
) -> anyhow::Result<Option<StoredDefinitionOwner>> {
    tx.query_opt(
        SELECT_DEFINITION_OWNER_SQL,
        &[&env_key, &schema, &relation, &kind.as_str(), &definition],
    )
    .await
    .context("read durable definition owner")
    .map(|row| {
        row.map(|row| StoredDefinitionOwner {
            package_id: row.get(0),
            client_field_extensible: row.get(1),
        })
    })
}

pub(super) async fn definition_present(
    tx: &Transaction<'_>,
    schema: &str,
    relation: &str,
    kind: DefinitionType,
    definition: &str,
) -> anyhow::Result<bool> {
    let row = match kind {
        DefinitionType::Relation => {
            tx.query_one(SELECT_RELATION_PRESENT_SQL, &[&schema, &relation])
                .await
        }
        DefinitionType::Field => {
            tx.query_one(SELECT_FIELD_PRESENT_SQL, &[&schema, &relation, &definition])
                .await
        }
        DefinitionType::Constraint => {
            tx.query_one(
                SELECT_CONSTRAINT_PRESENT_SQL,
                &[&schema, &relation, &definition],
            )
            .await
        }
    }
    .context("read server definition presence")?;
    Ok(row.get(0))
}

pub(super) fn model_for_relation<'a>(
    manifest: &'a PackageManifest,
    schema: &str,
    relation: &str,
) -> Option<&'a ModelDeclaration> {
    manifest
        .models
        .values()
        .find(|model| model.schema == schema && model.table == relation)
}

pub(super) fn definition_error(
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

fn definition_error_for_parts(
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

#[derive(Debug)]
pub(super) struct DeferredMigrationPolicyError {
    pub(super) relative_path: Box<str>,
    pub(super) source: MigrationPolicyError,
}

#[derive(Debug)]
pub(super) struct MigrationPolicyPlan {
    pub(super) mutations: Vec<PlannedDefinitionMutation>,
    pub(super) deferred: Option<DeferredMigrationPolicyError>,
}

pub(super) fn validate_migration_policy(
    package_root: &Path,
    directory: &PackageDirectory,
    plan: &wamn_schema_control::PackageMigrationPlan,
) -> anyhow::Result<MigrationPolicyPlan> {
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
    Ok(MigrationPolicyPlan {
        mutations,
        deferred,
    })
}

fn validate_relation_classifications(
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

pub(super) async fn validate_definition_ownership_before_apply(
    tx: &Transaction<'_>,
    env_key: &str,
    coordinate: &str,
    package_id: &str,
    manifest: &PackageManifest,
    mutations: &[&PlannedDefinitionMutation],
) -> anyhow::Result<()> {
    validate_manifest_definition_owners(manifest)?;
    // One apply runs every pending migration in one transaction and records the
    // definition owners after them. A stream that creates a relation and then
    // adds to it therefore reads the owner this apply is about to record.
    let mut created = Vec::new();
    for planned in mutations {
        let mutation = &planned.mutation;
        match mutation.action() {
            DefinitionAction::Create => {
                preflight_create_relation(tx, env_key, coordinate, package_id, manifest, planned)
                    .await?;
                created.push((mutation.schema(), mutation.relation()));
            }
            DefinitionAction::Add => {
                preflight_add_definition(
                    tx,
                    env_key,
                    coordinate,
                    package_id,
                    manifest,
                    planned,
                    created.contains(&(mutation.schema(), mutation.relation())),
                )
                .await?;
            }
            DefinitionAction::Alter | DefinitionAction::Drop => {
                preflight_existing_definition_mutation(
                    tx, env_key, coordinate, package_id, planned,
                )
                .await?;
            }
        }
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

async fn preflight_create_relation(
    tx: &Transaction<'_>,
    env_key: &str,
    coordinate: &str,
    package_id: &str,
    manifest: &PackageManifest,
    planned: &PlannedDefinitionMutation,
) -> anyhow::Result<()> {
    let mutation = &planned.mutation;
    if let Some(owner) = load_definition_owner(
        tx,
        env_key,
        mutation.schema(),
        mutation.relation(),
        DefinitionType::Relation,
        mutation.relation(),
    )
    .await?
    {
        let type_ = if owner.package_id == package_id {
            ApplyPackageErrorType::DefinitionOwnerConflict
        } else {
            ApplyPackageErrorType::BaseDefinitionMutation
        };
        return Err(definition_error(
            type_,
            coordinate,
            planned,
            Some(owner.package_id.as_str()),
            "CREATE TABLE cannot replace an existing managed relation",
        )
        .into());
    }
    if definition_present(
        tx,
        mutation.schema(),
        mutation.relation(),
        DefinitionType::Relation,
        mutation.relation(),
    )
    .await?
    {
        return Err(definition_error(
            ApplyPackageErrorType::DefinitionOwnerConflict,
            coordinate,
            planned,
            None,
            "the live relation has no durable definition owner",
        )
        .into());
    }
    if let Some(model) = model_for_relation(manifest, mutation.schema(), mutation.relation())
        && model.owner != package_id
    {
        return Err(definition_error(
            ApplyPackageErrorType::DefinitionOwnerDeclarationMissing,
            coordinate,
            planned,
            Some(model.owner.as_str()),
            "a package may create only a relation it declares as its own",
        )
        .into());
    }
    Ok(())
}

async fn preflight_add_definition(
    tx: &Transaction<'_>,
    env_key: &str,
    coordinate: &str,
    package_id: &str,
    manifest: &PackageManifest,
    planned: &PlannedDefinitionMutation,
    created_by_this_apply: bool,
) -> anyhow::Result<()> {
    let mutation = &planned.mutation;
    let stored = load_definition_owner(
        tx,
        env_key,
        mutation.schema(),
        mutation.relation(),
        DefinitionType::Relation,
        mutation.relation(),
    )
    .await?;
    // An earlier migration of this apply creates the relation, and the apply
    // records the same owner row that this branch reads.
    let relation_owner = match stored {
        Some(owner) => owner,
        None if created_by_this_apply => StoredDefinitionOwner {
            package_id: package_id.to_owned(),
            client_field_extensible: model_for_relation(
                manifest,
                mutation.schema(),
                mutation.relation(),
            )
            .filter(|model| model.owner == package_id)
            .is_some_and(|model| model.client_field_extensible),
        },
        None => {
            return Err(definition_error(
                ApplyPackageErrorType::DefinitionOwnerConflict,
                coordinate,
                planned,
                None,
                "the target relation has no durable definition owner",
            )
            .into());
        }
    };

    if relation_owner.package_id != package_id {
        if !relation_owner.client_field_extensible {
            return Err(definition_error(
                ApplyPackageErrorType::RelationNotClientExtensible,
                coordinate,
                planned,
                Some(relation_owner.package_id.as_str()),
                "the base package must declare client_field_extensible before an overlay adds definitions",
            )
            .into());
        }
        let Some(model) = model_for_relation(manifest, mutation.schema(), mutation.relation())
        else {
            return Err(definition_error(
                ApplyPackageErrorType::DefinitionOwnerDeclarationMissing,
                coordinate,
                planned,
                Some(relation_owner.package_id.as_str()),
                "the overlay manifest must name the shared relation and its base owner",
            )
            .into());
        };
        if model.owner != relation_owner.package_id
            || explicit_definition_owner(model, mutation.definition_type(), mutation.definition())
                != Some(package_id)
        {
            return Err(definition_error(
                ApplyPackageErrorType::DefinitionOwnerDeclarationMissing,
                coordinate,
                planned,
                Some(relation_owner.package_id.as_str()),
                "an additive shared-relation definition must explicitly name the applying package as owner",
            )
            .into());
        }
    } else if let Some(model) = model_for_relation(manifest, mutation.schema(), mutation.relation())
        && explicit_definition_owner(model, mutation.definition_type(), mutation.definition())
            .is_some_and(|owner| owner != package_id)
    {
        return Err(definition_error(
            ApplyPackageErrorType::DefinitionOwnerDeclarationMissing,
            coordinate,
            planned,
            explicit_definition_owner(model, mutation.definition_type(), mutation.definition()),
            "a package cannot add a definition declared as another package's property",
        )
        .into());
    }

    if let Some(owner) = load_definition_owner(
        tx,
        env_key,
        mutation.schema(),
        mutation.relation(),
        mutation.definition_type(),
        mutation.definition(),
    )
    .await?
    {
        let type_ = if owner.package_id == package_id {
            ApplyPackageErrorType::DefinitionOwnerConflict
        } else {
            ApplyPackageErrorType::BaseDefinitionMutation
        };
        return Err(definition_error(
            type_,
            coordinate,
            planned,
            Some(owner.package_id.as_str()),
            "ADD cannot replace an existing managed definition",
        )
        .into());
    }
    if definition_present(
        tx,
        mutation.schema(),
        mutation.relation(),
        mutation.definition_type(),
        mutation.definition(),
    )
    .await?
    {
        return Err(definition_error(
            ApplyPackageErrorType::DefinitionOwnerConflict,
            coordinate,
            planned,
            None,
            "the live definition has no durable definition owner",
        )
        .into());
    }
    Ok(())
}

async fn preflight_existing_definition_mutation(
    tx: &Transaction<'_>,
    env_key: &str,
    coordinate: &str,
    package_id: &str,
    planned: &PlannedDefinitionMutation,
) -> anyhow::Result<()> {
    let mutation = &planned.mutation;
    if let Some(owner) = load_definition_owner(
        tx,
        env_key,
        mutation.schema(),
        mutation.relation(),
        mutation.definition_type(),
        mutation.definition(),
    )
    .await?
    {
        if owner.package_id != package_id {
            return Err(definition_error(
                ApplyPackageErrorType::BaseDefinitionMutation,
                coordinate,
                planned,
                Some(owner.package_id.as_str()),
                "an overlay may not alter or drop a definition owned by its base",
            )
            .into());
        }
    } else if definition_present(
        tx,
        mutation.schema(),
        mutation.relation(),
        mutation.definition_type(),
        mutation.definition(),
    )
    .await?
    {
        return Err(definition_error(
            ApplyPackageErrorType::DefinitionOwnerConflict,
            coordinate,
            planned,
            None,
            "the live definition has no durable definition owner",
        )
        .into());
    }
    Ok(())
}

fn explicit_definition_owner<'a>(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_owners_must_name_the_package_or_a_declared_base() {
        let mut manifest = PackageManifest::from_slice(include_bytes!(
            "../../tests/fixtures/observer_package/wamn.json"
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

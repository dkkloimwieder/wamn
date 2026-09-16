use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
use wamn_record_history::{HISTORY_TABLE_SUFFIX, is_history_table_name};
use wamn_schema_control::PackageDirectory;
use wamn_schema_generator::{ModelDeclaration, PackageManifest};
use wamn_schema_introspection::migration_policy::{
    DefinitionAction, DefinitionKind, MigrationPolicyError, MigrationPolicyErrorKind,
    inspect_migration_definition_mutations,
};

use super::definition_ownership::{
    PlannedDefinitionMutation, StoredDefinitionOwner, definition_error, definition_present,
    load_definition_owner, model_for_relation,
};
use super::error::{ApplyPackageErrorKind, DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL};

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
        let validation =
            wamn_schema_introspection::migration_policy::validate_migration_bytes_for_schemas(
                &path,
                &migration.bytes,
                &schemas,
            );
        match validation {
            Ok(()) => {}
            Err(source)
                if source.kind() == MigrationPolicyErrorKind::UnsupportedStatement
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
    tenant: &str,
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
                preflight_create_relation(tx, tenant, coordinate, package_id, manifest, planned)
                    .await?;
                created.push((mutation.schema(), mutation.relation()));
            }
            DefinitionAction::Add => {
                preflight_add_definition(
                    tx,
                    tenant,
                    coordinate,
                    package_id,
                    manifest,
                    planned,
                    created.contains(&(mutation.schema(), mutation.relation())),
                )
                .await?;
            }
            DefinitionAction::Alter | DefinitionAction::Drop => {
                preflight_existing_definition_mutation(tx, tenant, coordinate, package_id, planned)
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
    tenant: &str,
    coordinate: &str,
    package_id: &str,
    manifest: &PackageManifest,
    planned: &PlannedDefinitionMutation,
) -> anyhow::Result<()> {
    let mutation = &planned.mutation;
    if let Some(owner) = load_definition_owner(
        tx,
        tenant,
        mutation.schema(),
        mutation.relation(),
        DefinitionKind::Relation,
        mutation.relation(),
    )
    .await?
    {
        let kind = if owner.package_id == package_id {
            ApplyPackageErrorKind::DefinitionOwnerConflict
        } else {
            ApplyPackageErrorKind::BaseDefinitionMutation
        };
        return Err(definition_error(
            kind,
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
        DefinitionKind::Relation,
        mutation.relation(),
    )
    .await?
    {
        return Err(definition_error(
            ApplyPackageErrorKind::DefinitionOwnerConflict,
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
            ApplyPackageErrorKind::DefinitionOwnerDeclarationMissing,
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
    tenant: &str,
    coordinate: &str,
    package_id: &str,
    manifest: &PackageManifest,
    planned: &PlannedDefinitionMutation,
    created_by_this_apply: bool,
) -> anyhow::Result<()> {
    let mutation = &planned.mutation;
    let stored = load_definition_owner(
        tx,
        tenant,
        mutation.schema(),
        mutation.relation(),
        DefinitionKind::Relation,
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
                ApplyPackageErrorKind::DefinitionOwnerConflict,
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
                ApplyPackageErrorKind::RelationNotClientExtensible,
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
                ApplyPackageErrorKind::DefinitionOwnerDeclarationMissing,
                coordinate,
                planned,
                Some(relation_owner.package_id.as_str()),
                "the overlay manifest must name the shared relation and its base owner",
            )
            .into());
        };
        if model.owner != relation_owner.package_id
            || explicit_definition_owner(model, mutation.kind(), mutation.definition())
                != Some(package_id)
        {
            return Err(definition_error(
                ApplyPackageErrorKind::DefinitionOwnerDeclarationMissing,
                coordinate,
                planned,
                Some(relation_owner.package_id.as_str()),
                "an additive shared-relation definition must explicitly name the applying package as owner",
            )
            .into());
        }
    } else if let Some(model) = model_for_relation(manifest, mutation.schema(), mutation.relation())
        && explicit_definition_owner(model, mutation.kind(), mutation.definition())
            .is_some_and(|owner| owner != package_id)
    {
        return Err(definition_error(
            ApplyPackageErrorKind::DefinitionOwnerDeclarationMissing,
            coordinate,
            planned,
            explicit_definition_owner(model, mutation.kind(), mutation.definition()),
            "a package cannot add a definition declared as another package's property",
        )
        .into());
    }

    if let Some(owner) = load_definition_owner(
        tx,
        tenant,
        mutation.schema(),
        mutation.relation(),
        mutation.kind(),
        mutation.definition(),
    )
    .await?
    {
        let kind = if owner.package_id == package_id {
            ApplyPackageErrorKind::DefinitionOwnerConflict
        } else {
            ApplyPackageErrorKind::BaseDefinitionMutation
        };
        return Err(definition_error(
            kind,
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
        mutation.kind(),
        mutation.definition(),
    )
    .await?
    {
        return Err(definition_error(
            ApplyPackageErrorKind::DefinitionOwnerConflict,
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
    tenant: &str,
    coordinate: &str,
    package_id: &str,
    planned: &PlannedDefinitionMutation,
) -> anyhow::Result<()> {
    let mutation = &planned.mutation;
    if let Some(owner) = load_definition_owner(
        tx,
        tenant,
        mutation.schema(),
        mutation.relation(),
        mutation.kind(),
        mutation.definition(),
    )
    .await?
    {
        if owner.package_id != package_id {
            return Err(definition_error(
                ApplyPackageErrorKind::BaseDefinitionMutation,
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
        mutation.kind(),
        mutation.definition(),
    )
    .await?
    {
        return Err(definition_error(
            ApplyPackageErrorKind::DefinitionOwnerConflict,
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
    kind: DefinitionKind,
    definition: &str,
) -> Option<&'a str> {
    match kind {
        DefinitionKind::Relation => Some(model.owner.as_str()),
        DefinitionKind::Field => model.field_owners.get(definition).map(String::as_str),
        DefinitionKind::Constraint => model.constraint_owners.get(definition).map(String::as_str),
    }
}

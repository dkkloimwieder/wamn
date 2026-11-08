pub(super) mod plan;
pub(super) use plan::{definition_error, model_for_relation, validate_migration_policy};
use plan::{
    definition_error_for_parts, explicit_definition_owner, validate_manifest_definition_owners,
};

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
use wamn_catalog::manifest::{ModelDeclaration, PackageManifest};
use wamn_record_history::{HISTORY_TABLE_SUFFIX, is_history_table_name};
use wamn_schema_control::PackageDirectory;
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

pub(super) use plan::MigrationPolicyPlan;

pub(super) async fn validate_definition_ownership_before_apply(
    tx: &Transaction<'_>,
    env_key: &str,
    coordinate: &str,
    package_id: &str,
    manifest: &PackageManifest,
    mutations: &[&PlannedDefinitionMutation],
) -> anyhow::Result<()> {
    validate_manifest_definition_owners(manifest)?;
    // A file can create a relation and then add to it before its owners are
    // recorded. Later files read the owners already recorded by earlier files.
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

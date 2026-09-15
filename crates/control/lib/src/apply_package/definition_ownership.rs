use anyhow::Context as _;
use tokio_postgres::Transaction;
use wamn_schema_generator::{ModelDeclaration, PackageManifest};
use wamn_schema_introspection::migration_policy::{
    DefinitionAction, DefinitionKind, DefinitionMutation,
};

use super::error::{ApplyPackageError, ApplyPackageErrorKind};

const SELECT_DEFINITION_OWNER_SQL: &str = "\
SELECT owner_package_id, client_field_extensible \
  FROM catalog.package_definition_owners \
 WHERE tenant_id = $1 AND schema_name = $2 AND relation_name = $3 \
   AND definition_kind = $4 AND definition_name = $5";
const INSERT_DEFINITION_OWNER_SQL: &str = "\
INSERT INTO catalog.package_definition_owners \
    (tenant_id, schema_name, relation_name, definition_kind, definition_name, \
     owner_package_id, client_field_extensible) \
VALUES ($1, $2, $3, $4, $5, $6, $7) \
ON CONFLICT (tenant_id, schema_name, relation_name, definition_kind, definition_name) \
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
SELECT definition_kind, definition_name FROM (\
    SELECT 'field'::text AS definition_kind, field.attname::text AS definition_name, \
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
) AS definitions ORDER BY ordering, definition_kind, definition_name COLLATE \"C\"";

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
    tenant: &str,
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
                    tenant,
                    coordinate,
                    planned,
                    DefinitionKind::Relation,
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
                        "field" => DefinitionKind::Field,
                        "constraint" => DefinitionKind::Constraint,
                        value => unreachable!("closed server definition kind {value}"),
                    };
                    let definition = row.get::<_, String>(1);
                    changed |= insert_definition_owner(
                        tx,
                        tenant,
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
                    tenant,
                    coordinate,
                    planned,
                    mutation.kind(),
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
        mutation.kind(),
        mutation.definition(),
    )
    .await?
    {
        Ok(())
    } else {
        Err(definition_error(
            ApplyPackageErrorKind::DefinitionNotFound,
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
    tenant: &str,
    coordinate: &str,
    planned: &PlannedDefinitionMutation,
    kind: DefinitionKind,
    definition: &str,
    package_id: &str,
    client_field_extensible: bool,
) -> anyhow::Result<bool> {
    let mutation = &planned.mutation;
    let inserted = tx
        .execute(
            INSERT_DEFINITION_OWNER_SQL,
            &[
                &tenant,
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
        tenant,
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
            ApplyPackageErrorKind::DefinitionOwnerConflict,
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
    tenant: &str,
    schema: &str,
    relation: &str,
    kind: DefinitionKind,
    definition: &str,
) -> anyhow::Result<Option<StoredDefinitionOwner>> {
    tx.query_opt(
        SELECT_DEFINITION_OWNER_SQL,
        &[&tenant, &schema, &relation, &kind.as_str(), &definition],
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
    kind: DefinitionKind,
    definition: &str,
) -> anyhow::Result<bool> {
    let row = match kind {
        DefinitionKind::Relation => {
            tx.query_one(SELECT_RELATION_PRESENT_SQL, &[&schema, &relation])
                .await
        }
        DefinitionKind::Field => {
            tx.query_one(SELECT_FIELD_PRESENT_SQL, &[&schema, &relation, &definition])
                .await
        }
        DefinitionKind::Constraint => {
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
    kind: ApplyPackageErrorKind,
    coordinate: &str,
    planned: &PlannedDefinitionMutation,
    owner_package: Option<&str>,
    detail: impl Into<String>,
) -> ApplyPackageError {
    definition_error_for_parts(
        kind,
        coordinate,
        planned,
        planned.mutation.kind(),
        planned.mutation.definition(),
        owner_package,
        detail,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the arguments preserve exact refusal context at the effect boundary"
)]
fn definition_error_for_parts(
    kind: ApplyPackageErrorKind,
    coordinate: &str,
    planned: &PlannedDefinitionMutation,
    definition_kind: DefinitionKind,
    definition: &str,
    owner_package: Option<&str>,
    detail: impl Into<String>,
) -> ApplyPackageError {
    ApplyPackageError {
        kind,
        coordinate: coordinate.to_owned(),
        predecessor_version: None,
        current_version: None,
        path: Some(planned.relative_path.to_string()),
        schema: Some(planned.mutation.schema().to_owned()),
        relation: Some(planned.mutation.relation().to_owned()),
        definition_kind: Some(definition_kind),
        definition: Some(definition.to_owned()),
        owner_package: owner_package.map(str::to_owned),
        detail: detail.into(),
        source: None,
    }
}

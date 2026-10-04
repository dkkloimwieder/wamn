//! Transactional conditions for package-owned online expand stages.

use std::collections::BTreeMap;

use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
use wamn_schema_introspection::{
    ir::postgres_type,
    migration_policy::{DefinitionAction, DefinitionType, StageRelation, validate_stage_condition},
};

use super::definition_ownership::PlannedDefinitionMutation;
use super::roles::{reset_host_role, set_package_owner_role};

/// Limit the whole-row exception to nullable additions without a default.
pub(super) async fn require_nullable_additions(
    tx: &Transaction<'_>,
    mutations: &[&PlannedDefinitionMutation],
) -> anyhow::Result<()> {
    for planned in mutations {
        let mutation = &planned.mutation;
        let admitted: bool = tx
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid=a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             WHERE n.nspname=$1 AND c.relname=$2 AND a.attname=$3 \
               AND a.attnum>0 AND NOT a.attisdropped AND NOT a.attnotnull \
               AND NOT a.atthasdef AND a.attgenerated='' AND a.attidentity='')",
                &[
                    &mutation.schema(),
                    &mutation.relation(),
                    &mutation.definition(),
                ],
            )
            .await?
            .try_get(0)?;
        ensure!(
            admitted,
            "whole_row_grants refuses {} statement {}: {}.{}.{} must be a nullable column without a default",
            planned.relative_path,
            mutation.statement_index(),
            mutation.schema(),
            mutation.relation(),
            mutation.definition(),
        );
    }
    Ok(())
}

/// Keep staged DDL within the implemented, expression-free column addition path.
pub(super) async fn require_owned_column_additions(
    tx: &Transaction<'_>,
    tenant: &str,
    package: &str,
    mutations: &[&PlannedDefinitionMutation],
) -> anyhow::Result<()> {
    for planned in mutations {
        let mutation = &planned.mutation;
        ensure!(
            mutation.action() == DefinitionAction::Add
                && mutation.definition_type() == DefinitionType::Field,
            "stage migration {} statement {} on {}.{} requires an unimplemented staged DDL path",
            planned.relative_path,
            mutation.statement_index(),
            mutation.schema(),
            mutation.relation()
        );
        let owned: bool = tx
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM catalog.package_definition_owners \
             WHERE tenant_id=$1 AND schema_name=$2 AND relation_name=$3 \
               AND definition_type='relation' AND owner_package_id=$4)",
                &[&tenant, &mutation.schema(), &mutation.relation(), &package],
            )
            .await?
            .try_get(0)?;
        ensure!(
            owned,
            "stage relation {}.{} is not owned by package {package}",
            mutation.schema(),
            mutation.relation()
        );
    }
    Ok(())
}

/// Read owned ordinary relations and their actual scalar column types.
pub(super) async fn owned_relations(
    tx: &Transaction<'_>,
    tenant: &str,
    package: &str,
    schema: &str,
) -> anyhow::Result<Vec<StageRelation>> {
    let rows = tx.query(
        "SELECT n.nspname,c.relname,a.attname,pg_catalog.format_type(a.atttypid,NULL) AS scalar_type \
         FROM catalog.package_definition_owners o \
         JOIN pg_catalog.pg_namespace n ON n.nspname=o.schema_name \
         JOIN pg_catalog.pg_class c ON c.relnamespace=n.oid AND c.relname=o.relation_name AND c.relkind='r' \
         JOIN pg_catalog.pg_attribute a ON a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped \
         JOIN pg_catalog.pg_type t ON t.oid=a.atttypid \
         JOIN pg_catalog.pg_namespace tn ON tn.oid=t.typnamespace \
         WHERE o.tenant_id=$1 AND o.owner_package_id=$2 AND o.definition_type='relation' \
           AND n.nspname=$3 AND tn.nspname='pg_catalog' \
         ORDER BY n.nspname,c.relname,a.attnum",
        &[&tenant, &package, &schema],
    ).await.context("read package-owned stage relations")?;
    let mut relations: BTreeMap<String, StageRelation> = BTreeMap::new();
    for row in rows {
        let name: String = row.try_get("relname")?;
        let relation = relations
            .entry(name.clone())
            .or_insert_with(|| StageRelation {
                schema: schema.to_owned(),
                name,
                columns: BTreeMap::new(),
            });
        let column: String = row.try_get("attname")?;
        let scalar: String = row.try_get("scalar_type")?;
        relation.columns.insert(column, postgres_type(&scalar)?);
    }
    Ok(relations.into_values().collect())
}

/// Evaluate named conditions using current ownership, within the migration transaction.
pub(super) async fn conditions(
    tx: &Transaction<'_>,
    tenant: &str,
    package: &str,
    schema: &str,
    section: &str,
    conditions: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let relations = owned_relations(tx, tenant, package, schema).await?;
    // Validate the complete condition set before executing any package SQL.
    for (name, sql) in conditions {
        validate_stage_condition(
            format!("upgrade_stage.{section}.{name}.sql"),
            sql.as_bytes(),
            &relations,
            &[],
        )?;
    }
    let previous: String = tx.query_one("SHOW search_path", &[]).await?.try_get(0)?;
    let search_path = format!("pg_catalog, {}, pg_temp", wamn_pg_core::quote_ident(schema));
    tx.batch_execute("SET LOCAL standard_conforming_strings = on")
        .await?;
    tx.query_one("SELECT set_config('search_path',$1,true)", &[&search_path])
        .await?;
    for (name, sql) in conditions {
        tx.batch_execute("SET LOCAL lock_timeout = '5s'").await?;
        set_package_owner_role(tx).await?;
        let rows = tx.query(sql, &[]).await.with_context(|| {
            format!("stage {section} {name} failed; stop and resume after correcting the cause")
        })?;
        reset_host_role(tx).await?;
        ensure!(
            rows.len() == 1 && rows[0].len() == 1,
            "stage {section} {name} must return exactly one boolean value"
        );
        ensure!(
            rows[0].try_get::<_, Option<bool>>(0)? == Some(true),
            "stage {section} {name} is not true"
        );
    }
    tx.query_one("SELECT set_config('search_path',$1,true)", &[&previous])
        .await?;
    Ok(())
}

use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
use wamn_control_provision::package_owner_role;

const SELECT_ROLE_CONTEXT_SQL: &str = "SELECT current_user::text, session_user::text";

pub(super) async fn set_package_owner_role(
    tx: &Transaction<'_>,
    package: &str,
) -> anyhow::Result<()> {
    let role = wamn_pg_core::quote_ident(&package_owner_role(package));
    tx.batch_execute(&format!("SET LOCAL ROLE {role}"))
        .await
        .with_context(|| format!("select owner role of package {package}"))
}

pub(super) async fn reset_host_role(tx: &Transaction<'_>) -> anyhow::Result<()> {
    tx.batch_execute("RESET ROLE")
        .await
        .context("reset package migration authority before trusted writes")?;
    assert_host_role(tx).await
}

pub(super) async fn assert_host_role(tx: &Transaction<'_>) -> anyhow::Result<()> {
    let row = tx
        .query_one(SELECT_ROLE_CONTEXT_SQL, &[])
        .await
        .context("read server role context before trusted package writes")?;
    let current_role = row.get::<_, String>(0);
    let session_role = row.get::<_, String>(1);
    ensure!(
        current_role == session_role,
        "package-role-reset-refused: current role {current_role:?} differs from session role {session_role:?}"
    );
    Ok(())
}

/// Run each admitted statement under its relation owner. An overlay can extend
/// a base relation only after the migration policy checks its field ownership.
/// Exact byte slices retain the authored statement and its literals.
pub(super) async fn execute_migration(
    tx: &Transaction<'_>,
    manifest: &wamn_schema_generator::PackageManifest,
    statement: &wamn_schema_control::SqlStatement,
) -> anyhow::Result<()> {
    use pg_query::NodeEnum;
    let parsed = pg_query::parse(&statement.sql).context("parse admitted package migration")?;
    for raw in &parsed.protobuf.stmts {
        let relation = match raw.stmt.as_ref().and_then(|stmt| stmt.node.as_ref()) {
            Some(NodeEnum::CreateStmt(stmt)) => stmt.relation.as_ref(),
            Some(NodeEnum::AlterTableStmt(stmt)) => stmt.relation.as_ref(),
            Some(NodeEnum::IndexStmt(stmt)) => stmt.relation.as_ref(),
            _ => None,
        };
        let owner = relation
            .and_then(|relation| {
                manifest.models.values().find(|model| {
                    model.schema == relation.schemaname && model.table == relation.relname
                })
            })
            .map_or(manifest.package.id.as_str(), |model| model.owner.as_str());
        // REFERENCES is needed only while an admitted foreign key is created.
        // Keep existing grants and remove each temporary grant before commit.
        let mut references = std::collections::BTreeSet::new();
        foreign_key_tables(&serde_json::to_value(raw)?, &mut references);
        let role = package_owner_role(owner);
        let mut granted = Vec::new();
        for table in references {
            let allowed: bool = tx.query_one(
                "SELECT CASE WHEN to_regclass($2::text) IS NULL THEN true ELSE has_table_privilege($1::text, $2::text, 'REFERENCES') END",
                &[&role, &table],
            ).await?.get(0);
            if !allowed {
                tx.batch_execute(&format!(
                    "GRANT REFERENCES ON TABLE {table} TO {}",
                    wamn_pg_core::quote_ident(&role)
                ))
                .await?;
                granted.push(table);
            }
        }
        set_package_owner_role(tx, owner).await?;
        let start = usize::try_from(raw.stmt_location)?;
        let end = if raw.stmt_len == 0 {
            statement.sql.len()
        } else {
            start + usize::try_from(raw.stmt_len)?
        };
        tx.batch_execute(&statement.sql[start..end])
            .await
            .with_context(|| format!("apply {} as package {owner}", statement.summary))?;
        reset_host_role(tx).await?;
        for table in granted {
            tx.batch_execute(&format!(
                "REVOKE REFERENCES ON TABLE {table} FROM {}",
                wamn_pg_core::quote_ident(&role)
            ))
            .await?;
        }
    }
    Ok(())
}

/// PostgreSQL's constraint node names the referenced relation in `pktable`.
fn foreign_key_tables(value: &serde_json::Value, tables: &mut std::collections::BTreeSet<String>) {
    match value {
        serde_json::Value::Object(fields) => {
            if let Some(table) = fields.get("pktable")
                && let (Some(schema), Some(relation)) =
                    (table["schemaname"].as_str(), table["relname"].as_str())
            {
                let relation = wamn_pg_core::quote_ident(relation);
                tables.insert(if schema.is_empty() {
                    relation
                } else {
                    format!("{}.{relation}", wamn_pg_core::quote_ident(schema))
                });
            }
            for field in fields.values() {
                foreign_key_tables(field, tables);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                foreign_key_tables(item, tables);
            }
        }
        _ => {}
    }
}

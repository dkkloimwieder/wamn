pub(super) mod plan;
use plan::foreign_key_tables;

use anyhow::{Context as _, ensure};
use tokio_postgres::{GenericClient, Transaction};
use wamn_control_provision::package_owner_role;

const SELECT_ROLE_CONTEXT_SQL: &str = "SELECT current_user::text, session_user::text";

pub(super) async fn set_package_owner_role(
    tx: &Transaction<'_>,
    package: &str,
) -> anyhow::Result<()> {
    let role = postgres_protocol::escape::escape_identifier(&package_owner_role(package));
    tx.batch_execute(&format!("SET LOCAL ROLE {role}"))
        .await
        .with_context(|| format!("select owner role of package {package}"))
}

pub(super) async fn reset_host_role(tx: &impl GenericClient) -> anyhow::Result<()> {
    tx.batch_execute("RESET ROLE")
        .await
        .context("reset package migration authority before trusted writes")?;
    assert_host_role(tx).await
}

pub(super) async fn assert_host_role(tx: &impl GenericClient) -> anyhow::Result<()> {
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
    tx: &impl GenericClient,
    manifest: &wamn_catalog::manifest::PackageManifest,
    statement: &wamn_schema_control::SqlStatement,
    transactional: bool,
) -> anyhow::Result<()> {
    let parsed = pg_query::parse(&statement.sql).context("parse admitted package migration")?;
    for raw in &parsed.protobuf.stmts {
        let owner = wamn_schema_control::physical_schema::migration_owner(manifest, raw);
        ensure!(
            transactional || owner == manifest.package.id,
            "nontransactional SQL must execute as its own package: {}",
            manifest.package.id
        );
        // REFERENCES is needed only while an admitted foreign key is created.
        // Keep existing grants and remove each temporary grant before commit.
        let mut references = std::collections::BTreeSet::new();
        foreign_key_tables(&serde_json::to_value(raw)?, &mut references);
        let role = package_owner_role(owner);
        let mut granted = Vec::new();
        let provenance_usage = (references.contains("\"platform\".\"external_system\"")
            || references.contains("\"platform\".\"file\""))
            && !tx
                .query_one(
                    "SELECT has_schema_privilege($1::text, 'platform', 'USAGE')",
                    &[&role],
                )
                .await?
                .get::<_, bool>(0);
        if provenance_usage {
            ensure!(
                transactional,
                "nontransactional SQL cannot borrow platform REFERENCES privileges"
            );
            tx.batch_execute(&format!(
                "GRANT USAGE ON SCHEMA platform TO {}",
                postgres_protocol::escape::escape_identifier(&role)
            ))
            .await?;
        }
        for table in references {
            let allowed: bool = tx.query_one(
                "SELECT CASE WHEN to_regclass($2::text) IS NULL THEN true ELSE has_table_privilege($1::text, $2::text, 'REFERENCES') END",
                &[&role, &table],
            ).await?.get(0);
            if !allowed {
                ensure!(
                    transactional,
                    "nontransactional SQL cannot borrow REFERENCES privileges on {table}"
                );
                tx.batch_execute(&format!(
                    "GRANT REFERENCES ON TABLE {table} TO {}",
                    postgres_protocol::escape::escape_identifier(&role)
                ))
                .await?;
                granted.push(table);
            }
        }
        let quoted_role = postgres_protocol::escape::escape_identifier(&role);
        let target = if transactional { "LOCAL " } else { "" };
        tx.batch_execute(&format!("SET {target}ROLE {quoted_role}"))
            .await
            .with_context(|| format!("select owner role of package {owner}"))?;
        let start = usize::try_from(raw.stmt_location)?;
        let end = if raw.stmt_len == 0 {
            statement.sql.len()
        } else {
            start + usize::try_from(raw.stmt_len)?
        };
        let result = tx
            .batch_execute(&statement.sql[start..end])
            .await
            .with_context(|| format!("apply {} as package {owner}", statement.summary));
        let cleanup = async {
            reset_host_role(tx).await?;
            if provenance_usage {
                tx.batch_execute(&format!(
                    "REVOKE USAGE ON SCHEMA platform FROM {}",
                    postgres_protocol::escape::escape_identifier(&role)
                ))
                .await?;
            }
            for table in granted {
                tx.batch_execute(&format!(
                    "REVOKE REFERENCES ON TABLE {table} FROM {}",
                    postgres_protocol::escape::escape_identifier(&role)
                ))
                .await?;
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        result?;
        cleanup?;
    }
    Ok(())
}

use sqlx::Row as _;
pub(super) mod plan;
use plan::foreign_key_tables;

use anyhow::{Context as _, ensure};

use wamn_control_provision::package_owner_role;

const SELECT_ROLE_CONTEXT_SQL: &str = "SELECT current_user::text, session_user::text";

pub(super) async fn set_package_owner_role(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    package: &str,
) -> anyhow::Result<()> {
    let role = postgres_protocol::escape::escape_identifier(&package_owner_role(package));
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET LOCAL ROLE {role}")))
        .execute(&mut **tx)
        .await
        .map(drop)
        .with_context(|| format!("select owner role of package {package}"))
}

pub(super) async fn reset_host_role(tx: &mut sqlx::PgConnection) -> anyhow::Result<()> {
    sqlx::raw_sql("RESET ROLE")
        .execute(&mut *tx)
        .await
        .map(drop)
        .context("reset package migration authority before trusted writes")?;
    assert_host_role(tx).await
}

pub(super) async fn assert_host_role(tx: &mut sqlx::PgConnection) -> anyhow::Result<()> {
    let row = sqlx::query(SELECT_ROLE_CONTEXT_SQL)
        .fetch_one(&mut *tx)
        .await
        .context("read server role context before trusted package writes")?;
    let current_role = row.get::<String, _>(0);
    let session_role = row.get::<String, _>(1);
    ensure!(
        current_role == session_role,
        "package-role-reset-refused: current role {current_role:?} differs from session role {session_role:?}"
    );
    Ok(())
}

/// Run each admitted statement under its relation owner. An overlay can extend
/// a base relation only after the migration policy tests its field ownership.
/// Exact byte slices retain the authored statement and its literals.
pub(super) async fn execute_migration(
    tx: &mut sqlx::PgConnection,
    manifest: &wamn_compiler::manifest::PackageManifest,
    statement: &wamn_schema_control::SqlStatement,
    transactional: bool,
) -> anyhow::Result<()> {
    let parsed = pg_query::parse(&statement.sql).context("parse admitted package migration")?;
    let compiled = manifest.into();
    for raw in &parsed.protobuf.stmts {
        let owner = wamn_schema_control::physical_schema::migration_owner(&compiled, raw);
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
            && !sqlx::query("SELECT has_schema_privilege($1::text, 'platform', 'USAGE')")
                .bind(&role)
                .fetch_one(&mut *tx)
                .await?
                .get::<bool, _>(0);
        if provenance_usage {
            ensure!(
                transactional,
                "nontransactional SQL cannot borrow platform REFERENCES privileges"
            );
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "GRANT USAGE ON SCHEMA platform TO {}",
                postgres_protocol::escape::escape_identifier(&role)
            )))
            .execute(&mut *tx)
            .await
            .map(drop)?;
        }
        for table in references {
            let allowed: bool = sqlx::query("SELECT CASE WHEN to_regclass($2::text) IS NULL THEN true ELSE has_table_privilege($1::text, $2::text, 'REFERENCES') END").bind(&role).bind(&table).fetch_one(&mut *tx).await?.get(0);
            if !allowed {
                ensure!(
                    transactional,
                    "nontransactional SQL cannot borrow REFERENCES privileges on {table}"
                );
                sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                    "GRANT REFERENCES ON TABLE {table} TO {}",
                    postgres_protocol::escape::escape_identifier(&role)
                )))
                .execute(&mut *tx)
                .await
                .map(drop)?;
                granted.push(table);
            }
        }
        let quoted_role = postgres_protocol::escape::escape_identifier(&role);
        let target = if transactional { "LOCAL " } else { "" };
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "SET {target}ROLE {quoted_role}"
        )))
        .execute(&mut *tx)
        .await
        .map(drop)
        .with_context(|| format!("select owner role of package {owner}"))?;
        let start = usize::try_from(raw.stmt_location)?;
        let end = if raw.stmt_len == 0 {
            statement.sql.len()
        } else {
            start + usize::try_from(raw.stmt_len)?
        };
        let result = sqlx::raw_sql(sqlx::AssertSqlSafe(statement.sql[start..end].to_string()))
            .execute(&mut *tx)
            .await
            .map(drop)
            .with_context(|| format!("apply {} as package {owner}", statement.summary));
        let cleanup = async {
            reset_host_role(tx).await?;
            if provenance_usage {
                sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                    "REVOKE USAGE ON SCHEMA platform FROM {}",
                    postgres_protocol::escape::escape_identifier(&role)
                )))
                .execute(&mut *tx)
                .await
                .map(drop)?;
            }
            for table in granted {
                sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                    "REVOKE REFERENCES ON TABLE {table} FROM {}",
                    postgres_protocol::escape::escape_identifier(&role)
                )))
                .execute(&mut *tx)
                .await
                .map(drop)?;
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        result?;
        cleanup?;
    }
    Ok(())
}

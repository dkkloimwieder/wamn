//! Image-owned platform SQL migrations.
use crate::migrations::MigrationDatabase;
use anyhow::ensure;
use sqlx::migrate::{Migration, Migrator};
pub fn embedded_sources(target: MigrationDatabase) -> Vec<Migration> {
    crate::migrations::embedded(target)
        .iter()
        .cloned()
        .collect()
}

pub async fn observe(
    client: &mut sqlx::PgConnection,
    database: MigrationDatabase,
    sources: &[Migration],
) -> anyhow::Result<wamn_postgres::migrations::SchemaReport> {
    let table = match database {
        MigrationDatabase::System => wamn_postgres::REGISTRY_HISTORY_TABLE,
        MigrationDatabase::Project => "platform._sqlx_migrations",
    };
    wamn_postgres::migrations::observe(client, table, sources).await
}
pub async fn observe_system(
    client: &mut sqlx::PgConnection,
) -> anyhow::Result<wamn_postgres::migrations::SchemaReport> {
    observe(
        client,
        MigrationDatabase::System,
        &embedded_sources(MigrationDatabase::System),
    )
    .await
}
pub async fn observe_project(
    client: &mut sqlx::PgConnection,
) -> anyhow::Result<wamn_postgres::migrations::SchemaReport> {
    observe(
        client,
        MigrationDatabase::Project,
        &embedded_sources(MigrationDatabase::Project),
    )
    .await
}

pub async fn apply_target(
    client: &mut sqlx::PgConnection,
    database: MigrationDatabase,
    sources: &[Migration],
) -> anyhow::Result<usize> {
    let (schema, lock) = match database {
        MigrationDatabase::System => (
            "registry",
            "SELECT pg_advisory_xact_lock(hashtextextended('wamn.platform.system-schema:' || current_database(), 0))",
        ),
        MigrationDatabase::Project => (
            "platform",
            "SELECT pg_advisory_xact_lock(hashtextextended('wamn.package.data-access:' || current_database(), 0))",
        ),
    };
    let login = database.require_login(client).await?;
    let owner = postgres_protocol::escape::escape_identifier(&database.owner_role());
    sqlx::query("INSERT INTO platform.login_principal(login,principal_id) VALUES($1,$2) ON CONFLICT(login) DO UPDATE SET principal_id=EXCLUDED.principal_id")
        .bind(login.as_str())
        .bind(sqlx::types::Uuid::from(crate::PlatformComponent::ApplyPackage.principal_id()))
        .execute(&mut *client).await?;
    let key = wamn_postgres::migrations::lock_session(client, lock, &[]).await?;
    let result = async {
        let table = format!("{schema}._sqlx_migrations");
        crate::migration::prepare_history(client, &table, &login).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE {table} OWNER TO {owner}"
        )))
        .execute(&mut *client)
        .await?;
        let migrator = Migrator::with_migrations(sources.to_vec());
        wamn_postgres::migrations::run(client, migrator, table).await
    }
    .await;
    // Connections are dedicated. A cancellation closes them; a completed call releases exclusion.
    let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .fetch_one(client)
        .await?;
    ensure!(unlocked, "platform schema session lock disappeared");
    result
}

/// Bootstrap the project schemas in one transaction as the database owner.
pub async fn install_project(client: &mut sqlx::PgConnection) -> anyhow::Result<bool> {
    use sqlx::Connection as _;

    MigrationDatabase::Project.require_login(client).await?;
    let mut tx = client.begin().await?;
    let present: bool = sqlx::query_scalar(r#"SELECT to_regclass('platform."user"') IS NOT NULL"#)
        .fetch_one(&mut *tx)
        .await?;
    if !present {
        for sql in [
            crate::CATALOG_SCHEMA_SQL,
            crate::APP_SCHEMA_SQL,
            include_str!("../../../deploy/sql/run-state.sql"),
            include_str!("../../../deploy/sql/run-queue.sql"),
        ] {
            sqlx::raw_sql(sql).execute(&mut *tx).await?;
        }
        for surface in [
            crate::sql::grant_dynamic_sql_surface_sql(),
            crate::sql::grant_platform_surface_sql(),
        ] {
            sqlx::raw_sql(sqlx::AssertSqlSafe(surface))
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(!present)
}

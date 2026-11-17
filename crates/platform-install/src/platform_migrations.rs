//! Image-owned platform SQL migrations.
use anyhow::ensure;
use sqlx::migrate::{Migration, Migrator};
use wamn_provision::migrations::MigrationDatabase;
pub fn embedded_sources(target: MigrationDatabase) -> Vec<Migration> {
    wamn_provision::migrations::embedded(target)
        .iter()
        .cloned()
        .collect()
}

pub async fn observe(
    client: &mut sqlx::PgConnection,
    database: MigrationDatabase,
    sources: &[Migration],
) -> anyhow::Result<crate::migrations::SchemaReport> {
    let table = match database {
        MigrationDatabase::System => wamn_registry::migrations::HISTORY_TABLE,
        MigrationDatabase::Project => "platform._sqlx_migrations",
    };
    crate::migrations::observe(client, table, sources).await
}
pub async fn observe_system(
    client: &mut sqlx::PgConnection,
) -> anyhow::Result<crate::migrations::SchemaReport> {
    observe(
        client,
        MigrationDatabase::System,
        &embedded_sources(MigrationDatabase::System),
    )
    .await
}
pub async fn observe_project(
    client: &mut sqlx::PgConnection,
) -> anyhow::Result<crate::migrations::SchemaReport> {
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
    let database_name: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&mut *client)
        .await?;
    let owner = postgres_protocol::escape::escape_identifier(&database.owner_role(&database_name));
    sqlx::query("INSERT INTO platform.login_principal(login,principal_id) VALUES($1,$2) ON CONFLICT(login) DO UPDATE SET principal_id=EXCLUDED.principal_id")
        .bind(&login)
        .bind(wamn_provision::PlatformComponent::ApplyPackage.principal_id())
        .execute(&mut *client).await?;
    let key = crate::migrations::lock_session(client, lock, &[]).await?;
    let result = async {
        let table = format!("{schema}._sqlx_migrations");
        wamn_provision::migration::prepare_history(client, &table, &login).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE {table} OWNER TO {owner}"
        )))
        .execute(&mut *client)
        .await?;
        let migrator = Migrator::with_migrations(sources.to_vec());
        crate::migrations::run(client, migrator, table).await
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

/// Install an empty project's platform schemas without repairing an existing installation.
pub async fn install_project(client: &mut sqlx::PgConnection) -> anyhow::Result<bool> {
    let present: bool = sqlx::query_scalar("SELECT to_regclass('platform.\"user\"') IS NOT NULL")
        .fetch_one(&mut *client)
        .await?;
    if present {
        return Ok(false);
    }
    sqlx::raw_sql(include_str!("../../../deploy/sql/project-roles.sql"))
        .execute(&mut *client)
        .await?;
    let database: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&mut *client)
        .await?;
    let owner = wamn_provision::migration::login(&database);
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "ALTER DATABASE {} OWNER TO {}; SET ROLE {}",
        postgres_protocol::escape::escape_identifier(&database),
        postgres_protocol::escape::escape_identifier(owner.as_str()),
        postgres_protocol::escape::escape_identifier(owner.as_str()),
    )))
    .execute(&mut *client)
    .await?;
    let result = async {
        sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
            .execute(&mut *client)
            .await?;
        sqlx::raw_sql(wamn_provision::APP_SCHEMA_SQL)
            .execute(&mut *client)
            .await?;
        sqlx::raw_sql(include_str!("../../../deploy/sql/run-state.sql"))
            .execute(&mut *client)
            .await?;
        sqlx::raw_sql(include_str!("../../../deploy/sql/run-queue.sql"))
            .execute(&mut *client)
            .await?;
        for surface in [
            wamn_provision::sql::grant_dynamic_sql_surface_sql(),
            wamn_provision::sql::grant_platform_surface_sql(),
        ] {
            sqlx::raw_sql(sqlx::AssertSqlSafe(surface))
                .execute(&mut *client)
                .await?;
        }
        anyhow::Ok(())
    }
    .await;
    if result.is_err() {
        sqlx::raw_sql("ROLLBACK").execute(&mut *client).await?;
    }
    sqlx::raw_sql("RESET ROLE").execute(&mut *client).await?;
    result?;
    Ok(true)
}

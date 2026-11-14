//! Image-owned platform SQL migrations.
use anyhow::ensure;
use sqlx::migrate::{Migration, Migrator};
use wamn_control_provision::migrations::MigrationDatabase;
pub fn embedded_sources(target: MigrationDatabase) -> Vec<Migration> {
    wamn_control_provision::migrations::embedded(target)
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
        MigrationDatabase::System => "registry._sqlx_migrations",
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
    let (schema, owner, lock) = match database {
        MigrationDatabase::System => (
            "registry",
            "wamn_system",
            "SELECT pg_advisory_xact_lock(hashtextextended('wamn.platform.system-schema:' || current_database(), 0))",
        ),
        MigrationDatabase::Project => (
            "platform",
            "wamn_db_owner",
            "SELECT pg_advisory_xact_lock(hashtextextended('wamn.package.data-access:' || current_database(), 0))",
        ),
    };
    let login = database.require_login(client).await?;
    let key = crate::migrations::lock_session(client, lock, &[]).await?;
    let result = async {
        let component = wamn_control_provision::PlatformComponent::Provisioning;
        sqlx::query(
            "SELECT set_config('app.user_id',$1,false), set_config('app.operation',$2,false)",
        )
        .bind(component.principal_id().to_string())
        .bind(component.principal_name())
        .execute(&mut *client)
        .await?;
        let table = format!("{schema}._sqlx_migrations");
        wamn_control_provision::migration::prepare_history(client, &table, &login).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE {table} OWNER TO {owner}"
        )))
        .execute(&mut *client)
        .await?;
        if database == MigrationDatabase::System {
            sqlx::raw_sql("GRANT SELECT ON registry._sqlx_migrations TO wamn_apply")
                .execute(&mut *client)
                .await?;
        }
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

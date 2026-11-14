//! SQLx applies image-owned platform migrations under authenticated platform authority.

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

#[derive(Debug, Clone, Copy)]
pub enum MigrationTarget<'a> {
    System,
    Project(&'a crate::environment::lock::LifecycleLock),
}
impl MigrationTarget<'_> {
    fn database(self) -> MigrationDatabase {
        match self {
            Self::System => MigrationDatabase::System,
            Self::Project(_) => MigrationDatabase::Project,
        }
    }
}

pub async fn apply_reported(
    client: &mut sqlx::PgConnection,
    target: MigrationTarget<'_>,
    sources: &[Migration],
) -> anyhow::Result<usize> {
    apply_target(client, target.database(), sources).await
}

pub async fn restore_project(
    migration_input: &str,
    sources: &[Migration],
) -> anyhow::Result<usize> {
    let mut connection = wamn_postgres::connect(migration_input).await?;
    apply_target(&mut connection, MigrationDatabase::Project, sources).await
}

pub async fn run(
    client: &mut sqlx::PgConnection,
    target: MigrationTarget<'_>,
    sources: &[Migration],
    apply: bool,
    mut output: impl std::io::Write,
) -> anyhow::Result<()> {
    let database = target.database();
    database.require_login(client).await?;
    let report = observe(client, database, sources).await?;
    write!(output, "{report}")?;
    output.flush()?;
    if apply {
        apply_reported(client, target, sources).await?;
        write!(output, "{}", observe(client, database, sources).await?)?;
        output.flush()?;
    }
    Ok(())
}

async fn apply_target(
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
        MigrationDatabase::Project => ("platform", "wamn_db_owner", crate::apply_package::LOCK_SQL),
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

/// Install an empty project's platform schemas without repairing an existing installation.
pub async fn install_project(client: &mut sqlx::PgConnection) -> anyhow::Result<bool> {
    let present: bool = sqlx::query_scalar("SELECT to_regclass('platform.\"user\"') IS NOT NULL")
        .fetch_one(&mut *client)
        .await?;
    if present {
        return Ok(false);
    }
    for family in [
        wamn_control_provision::WorkloadRoleFamily::Retention,
        wamn_control_provision::WorkloadRoleFamily::AuditRetention,
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            wamn_control_provision::sql::ensure_workload_acl_role_sql(family),
        ))
        .execute(&mut *client)
        .await?;
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::ensure_platform_group_role_sql(),
    ))
    .execute(&mut *client)
    .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::ensure_app_acl_role_sql(),
    ))
    .execute(&mut *client)
    .await?;
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut *client)
        .await?;
    let result = async {
        sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
            .execute(&mut *client)
            .await?;
        sqlx::raw_sql(wamn_control_provision::APP_SCHEMA_SQL)
            .execute(&mut *client)
            .await?;
        sqlx::raw_sql(include_str!("../../../../deploy/sql/run-state.sql"))
            .execute(&mut *client)
            .await?;
        sqlx::raw_sql(include_str!("../../../../deploy/sql/run-queue.sql"))
            .execute(&mut *client)
            .await?;
        anyhow::Ok(())
    }
    .await;
    sqlx::raw_sql("RESET ROLE").execute(&mut *client).await?;
    result?;
    sqlx::raw_sql(wamn_control_provision::MIGRATION_PRIVILEGES_SQL)
        .execute(client)
        .await?;
    Ok(true)
}

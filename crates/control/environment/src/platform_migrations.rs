//! SQLx applies image-owned platform migrations under authenticated platform authority.

use sqlx::migrate::Migration;
use wamn_control_provision::migrations::MigrationDatabase;
use wamn_platform_install::platform_migrations::apply_target;

pub use wamn_platform_install::platform_migrations::{
    embedded_sources, observe, observe_project, observe_system,
};

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

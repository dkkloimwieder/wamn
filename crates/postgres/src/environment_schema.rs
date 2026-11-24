//! Independent schema provisioning for a project database.

use crate::LifecycleLock;
use wamn_registry::Triple;

/// Apply or inspect substrate SQL under the caller's environment lock.
pub async fn project_schema(
    lock: &LifecycleLock,
    triple: &Triple,
    migration_database_url: &str,
    sources: &[sqlx::migrate::Migration],
    apply: bool,
    output: impl std::io::Write,
) -> anyhow::Result<()> {
    super::environment_registry::require_connected(&mut *lock.client().await, triple).await?;
    let mut client = crate::connect(migration_database_url).await?;
    super::environment_schema::run(
        &mut client,
        super::environment_schema::MigrationTarget::Project(lock),
        sources,
        apply,
        output,
    )
    .await
}

use crate::migration_scope::MigrationDatabase;
use crate::platform_migrations::apply_target;
use sqlx::migrate::Migration;

pub use crate::platform_migrations::{embedded_sources, observe, observe_project, observe_system};

#[derive(Debug, Clone, Copy)]
pub enum MigrationTarget<'a> {
    System,
    Project(&'a crate::LifecycleLock),
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
    let mut connection = crate::connect(migration_input).await?;
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

pub use crate::platform_migrations::install_project;

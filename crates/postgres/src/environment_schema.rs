//! Independent schema provisioning for a project database.

use crate::migration_scope::MigrationDatabase;
use crate::platform_migrations::apply_target;
use sqlx::migrate::Migration;

pub use crate::platform_migrations::embedded_sources;

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

pub use crate::platform_migrations::install_project;

//! Independent schema provisioning for a project database.

pub mod reset;
pub mod schema;

use wamn_catalog::lifecycle_lock::LifecycleLock;
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
    crate::provision_project_env::require_connected(&mut *lock.client().await, triple).await?;
    let mut client = wamn_postgres::connect(migration_database_url).await?;
    crate::platform_migrations::run(
        &mut client,
        crate::platform_migrations::MigrationTarget::Project(lock),
        sources,
        apply,
        output,
    )
    .await
}

//! Independent schema provisioning for a project database.

use sqlx::migrate::Migration;
use wamn_postgres::migration_scope::MigrationDatabase;
use wamn_postgres::migrations::apply_target;

pub async fn restore_project(
    migration_input: &str,
    sources: &[Migration],
) -> anyhow::Result<usize> {
    let mut connection = wamn_postgres::connect(migration_input).await?;
    apply_target(&mut connection, MigrationDatabase::Project, sources).await
}

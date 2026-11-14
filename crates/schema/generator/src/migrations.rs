//! Native SQLx resolution of authored and generated migration directories.

use sqlx::migrate::Migrator;
use std::path::Path;

/// Resolve both directories through SQLx, including reversible files and ignored entries.
pub async fn migrator(root: &Path) -> anyhow::Result<Migrator> {
    let mut migrations = Vec::new();
    for relative in ["migrations", "history"] {
        let directory = root.join(relative);
        if directory.is_dir() {
            migrations.extend(Migrator::new(directory).await?.migrations.into_owned());
        }
    }
    let mut migrator = Migrator::with_migrations(migrations);
    migrator.set_locking(false);
    Ok(migrator)
}

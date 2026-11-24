//! Resolve package migration files with the same rules for build and deployment.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::ensure;
use sqlx::migrate::Migrator;

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
    let mut versions = BTreeSet::new();
    for migration in migrator
        .iter()
        .filter(|migration| migration.migration_type.is_up_migration())
    {
        ensure!(
            versions.insert(migration.version),
            "duplicate migration version {} in {}",
            migration.version,
            root.display()
        );
    }
    migrator.set_locking(false);
    Ok(migrator)
}

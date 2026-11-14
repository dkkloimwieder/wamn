//! Native SQLx resolution of authored and generated migration directories.

use anyhow::ensure;
use sqlx::migrate::Migrator;
use std::collections::BTreeSet;
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

/// Refuse additions below the highest version in the last completed build.
pub(crate) async fn validate_append(root: &Path, output: &Path) -> anyhow::Result<()> {
    let current = migrator(root).await?;
    if !output
        .join(crate::build_record::BUILD_RECORD_FILE)
        .is_file()
    {
        return Ok(());
    }
    crate::build_record::read(output)?;
    let previous = migrator(output).await?;
    let versions = previous
        .iter()
        .map(|migration| migration.version)
        .collect::<BTreeSet<_>>();
    if let Some(highest) = versions.last() {
        for migration in current.iter() {
            ensure!(
                versions.contains(&migration.version) || migration.version > *highest,
                "new migration version {} in {} must exceed {highest}",
                migration.version,
                root.display()
            );
        }
    }
    Ok(())
}

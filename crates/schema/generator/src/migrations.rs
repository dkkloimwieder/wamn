//! Native SQLx resolution of authored and generated migration directories.

use anyhow::{Context as _, ensure};
use sqlx::migrate::Migrator;
use std::collections::BTreeSet;
use std::path::Path;

pub(crate) const INPUT_VERSIONS_PREFIX: &str = "-- wamn-authored-migration-versions: ";

/// Refuse insertions below the immutable generated checkpoint, including uncommitted history.
pub(crate) async fn validate_history(root: &Path) -> anyhow::Result<()> {
    if !root.join("history").is_dir() {
        return Ok(());
    }
    let history = Migrator::new(root.join("history")).await?;
    let Some(latest) = history
        .iter()
        .filter(|migration| migration.migration_type.is_up_migration())
        .last()
    else {
        return Ok(());
    };
    let inputs = latest
        .sql
        .as_str()
        .lines()
        .next()
        .and_then(|line| line.strip_prefix(INPUT_VERSIONS_PREFIX))
        .context("history migration has no authored version checkpoint")?;
    let mut versions: BTreeSet<i64> =
        serde_json::from_str(inputs).context("history migration input versions")?;
    versions.extend(history.iter().map(|migration| migration.version));
    validate_versions(&migrator(root).await?, &versions, root)
}

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

/// Refuse additions below the highest version in an immutable source snapshot.
pub async fn validate_append(root: &Path, baseline: &Path) -> anyhow::Result<()> {
    let current = migrator(root).await?;
    let previous = migrator(baseline).await?;
    let versions = previous
        .iter()
        .map(|migration| migration.version)
        .collect::<BTreeSet<_>>();
    validate_versions(&current, &versions, root)
}

fn validate_versions(
    current: &Migrator,
    versions: &BTreeSet<i64>,
    root: &Path,
) -> anyhow::Result<()> {
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

/// Compare a frozen build input with the committed source, independent of build output.
pub(crate) async fn validate_committed(source: &Path, current: &Path) -> anyhow::Result<()> {
    use std::process::Command;
    let baseline = tempfile::tempdir()?;
    let head = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()?;
    if !head.status.success() {
        return validate_append(current, baseline.path()).await;
    }
    let tree = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["ls-tree", "-rz", "HEAD", "--", "migrations", "history"])
        .output()?;
    ensure!(
        tree.status.success(),
        "cannot read committed migration sources"
    );
    for entry in tree
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = std::str::from_utf8(entry).context("committed migration path is not UTF-8")?;
        let (object, path) = entry
            .split_once('\t')
            .context("invalid committed migration entry")?;
        let path = Path::new(path);
        if path.extension().is_none_or(|extension| extension != "sql") {
            continue;
        }
        let object = object
            .split_whitespace()
            .nth(2)
            .context("committed migration has no object")?;
        let bytes = Command::new("git")
            .arg("-C")
            .arg(source)
            .args(["cat-file", "blob", object])
            .output()?;
        ensure!(
            bytes.status.success(),
            "cannot read committed migration blob"
        );
        let destination = baseline.path().join(path);
        std::fs::create_dir_all(destination.parent().context("committed migration parent")?)?;
        std::fs::write(destination, bytes.stdout)?;
    }
    validate_append(current, baseline.path()).await
}

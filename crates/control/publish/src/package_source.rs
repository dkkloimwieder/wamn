//! Read immutable package inputs from source or verified artifacts.

use std::path::{Path, PathBuf};

use anyhow::Context as _;

use wamn_catalog::package_schema::{MigrationSource, PackageDirectory};

/// A package tree and the digest of the verified package artifact it was
/// opened from. The opener holds the digest in memory and passes it here; no
/// file in the tree carries it (docs/operations/building.md#reading-a-pushed-package). `None`
/// is a working tree, which only `wamn dev` applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRoot {
    pub path: PathBuf,
    pub artifact_digest: Option<String>,
}

/// Read the exact package bytes of `root`, with the artifact digest it carries.
pub fn read_package_root(root: &PackageRoot) -> anyhow::Result<PackageDirectory> {
    Ok(PackageDirectory {
        artifact_digest: root.artifact_digest.clone(),
        ..read_package_directory(&root.path)?
    })
}

/// Read the exact package bytes under `root`. The migrations are sorted by
/// file name, so the result does not depend on the file system order. A tree
/// carries no artifact digest, so the result has none
/// ([`read_package_root`] adds it).
pub fn read_package_directory(root: &Path) -> anyhow::Result<PackageDirectory> {
    let artifact_digest = None;
    let manifest_path = wamn_compiler::package_manifest_path(root);
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    read_migrations(&migration_root(root), manifest_bytes, artifact_digest)
}

pub fn migration_root(root: &Path) -> PathBuf {
    if wamn_compiler::is_authored(root) && !root.join("generated").is_dir() {
        wamn_compiler::output_root(root)
    } else {
        root.to_owned()
    }
}

fn read_migrations(
    root: &Path,
    manifest_bytes: Vec<u8>,
    artifact_digest: Option<String>,
) -> anyhow::Result<PackageDirectory> {
    let mut migrations = Vec::new();
    for relative in ["migrations", "history"] {
        let migrations_path = root.join(relative);
        if !migrations_path.exists() {
            continue;
        }
        let entries = std::fs::read_dir(&migrations_path)
            .with_context(|| format!("read {}", migrations_path.display()))?;
        for entry in entries {
            let entry = entry.context("read package migration directory entry")?;
            // SQLx ignores directories and unrelated files during native resolution.
            if !entry.path().is_file() {
                continue;
            }
            let file_name = entry.file_name().to_string_lossy().into_owned();
            migrations.push(MigrationSource {
                relative_path: format!("{relative}/{file_name}"),
                bytes: std::fs::read(entry.path())
                    .with_context(|| format!("read {}", entry.path().display()))?,
            });
        }
    }
    migrations.sort_unstable_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(PackageDirectory {
        manifest_bytes,
        migrations,
        artifact_digest,
    })
}

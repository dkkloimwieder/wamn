use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
use wamn_schema_control::{
    AppliedPackage, MigrationSource, PackageDirectory, RecordedMigration, plan_package_registration,
};

pub const LOCK_PACKAGE_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.package.lineage:' || $1 || ':' || $2, 0))";
const SELECT_PACKAGE_SQL: &str = "\
SELECT artifact_digest, predecessor_version FROM catalog.packages \
 WHERE env_key = $1 AND package_id = $2 AND package_version = $3 \
 FOR UPDATE";
const SELECT_MIGRATIONS_SQL: &str = "\
SELECT ordinal, relative_path, sha256 FROM catalog.package_migrations \
 WHERE env_key = $1 AND package_id = $2 AND package_version = $3 \
 ORDER BY ordinal";
pub const SELECT_CURRENT_PACKAGE_VERSION_SQL: &str = "\
SELECT package_version FROM catalog.packages \
 WHERE env_key = $1 AND package_id = $2";

/// Register a package while retaining its lineage lock through the caller's transaction.
pub async fn register_package(
    tx: &Transaction<'_>,
    env_key: &str,
    coordinate: &wamn_catalog::PackageCoordinate,
    artifact_digest: Option<&str>,
    predecessor_version: Option<&str>,
) -> anyhow::Result<bool> {
    let package_id = coordinate.package_id();
    tx.query_one(LOCK_PACKAGE_SQL, &[&env_key, &package_id])
        .await
        .context("lock package lineage before registration")?;
    let recorded = tx
        .query_opt(
            SELECT_PACKAGE_SQL,
            &[&env_key, &package_id, &coordinate.package_version()],
        )
        .await
        .context("read package coordinate before registration")?;
    let recorded = recorded.map(|row| {
        (
            row.get::<_, Option<String>>(0),
            row.get::<_, Option<String>>(1),
        )
    });
    let current = current_package_version(tx, env_key, package_id).await?;
    let insert = plan_package_registration(
        coordinate,
        artifact_digest,
        predecessor_version,
        recorded
            .as_ref()
            .map(|(digest, predecessor)| (digest.as_deref(), predecessor.as_deref())),
        current.as_deref(),
    )?;
    if insert {
        tx.execute(
            "INSERT INTO catalog.packages \
             (env_key, package_id, package_version, artifact_digest, predecessor_version) \
             VALUES ($1, $2, $3, $4, $5)",
            &[
                &env_key,
                &package_id,
                &coordinate.package_version(),
                &artifact_digest,
                &predecessor_version,
            ],
        )
        .await
        .context("insert immutable package root")?;
    }
    Ok(insert)
}

pub(super) async fn current_package_version(
    tx: &Transaction<'_>,
    env_key: &str,
    package_id: &str,
) -> anyhow::Result<Option<String>> {
    tx.query_opt(SELECT_CURRENT_PACKAGE_VERSION_SQL, &[&env_key, &package_id])
        .await
        .context("read current package-family leaf")
        .map(|row| row.map(|row| row.get(0)))
}

pub async fn load_applied_package(
    tx: &Transaction<'_>,
    env_key: &str,
    package_id: &str,
    package_version: &str,
) -> anyhow::Result<Option<AppliedPackage>> {
    let Some(package) = tx
        .query_opt(
            SELECT_PACKAGE_SQL,
            &[&env_key, &package_id, &package_version],
        )
        .await
        .context("read immutable package root")?
    else {
        return Ok(None);
    };
    let migrations = tx
        .query(
            SELECT_MIGRATIONS_SQL,
            &[&env_key, &package_id, &package_version],
        )
        .await
        .context("read immutable package migration prefix")?
        .into_iter()
        .map(|row| RecordedMigration {
            ordinal: u32::try_from(row.get::<_, i32>(0))
                .expect("package migration ordinals are positive integers"),
            relative_path: row.get(1),
            sha256: row.get(2),
        })
        .collect();
    Ok(Some(AppliedPackage {
        coordinate: wamn_catalog::PackageCoordinate::new(package_id, package_version)
            .expect("stored package coordinates passed database checks"),
        predecessor_version: package.get(1),
        artifact_digest: package.get(0),
        migrations,
    }))
}

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
    let manifest_path = wamn_schema_generator::package_manifest_path(root);
    // A hand-written source manifest does not consume package build output.
    if manifest_path.parent() == Some(wamn_schema_generator::output_root(root).as_path()) {
        wamn_schema_generator::build_record::check_current(root)?;
    }
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    read_migrations(root, manifest_bytes, artifact_digest)
}

/// Read authored inputs and migrations without consuming generated output.
pub(crate) fn read_source_package_directory(root: &Path) -> anyhow::Result<PackageDirectory> {
    if !wamn_schema_generator::is_authored(root) {
        return read_package_directory(root);
    }
    let manifest_bytes = wamn_schema_generator::compile_manifest(root)?;
    read_migrations(root, manifest_bytes, None)
}

fn read_migrations(
    root: &Path,
    manifest_bytes: Vec<u8>,
    artifact_digest: Option<String>,
) -> anyhow::Result<PackageDirectory> {
    let migrations_path = root.join("migrations");
    let mut migrations = Vec::new();
    // A package that owns no table has no migrations directory.
    if !migrations_path.exists()
        && wamn_catalog::manifest::PackageManifest::from_slice(&manifest_bytes)
            .is_ok_and(|manifest| manifest.owns_no_table())
    {
        return Ok(PackageDirectory {
            manifest_bytes,
            migrations,
            artifact_digest,
        });
    }
    let entries = std::fs::read_dir(&migrations_path)
        .with_context(|| format!("read {}", migrations_path.display()))?;
    for entry in entries {
        let entry = entry.context("read package migration directory entry")?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("inspect {}", entry.path().display()))?;
        ensure!(
            file_type.is_file(),
            "package migration entry is not a file: {}",
            entry.path().display()
        );
        let file_name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("package migration file name is not UTF-8"))?;
        migrations.push(MigrationSource {
            relative_path: format!("migrations/{file_name}"),
            bytes: std::fs::read(entry.path())
                .with_context(|| format!("read {}", entry.path().display()))?,
        });
    }
    migrations.sort_unstable_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(PackageDirectory {
        manifest_bytes,
        migrations,
        artifact_digest,
    })
}

#[cfg(test)]
mod directory_tests;
#[cfg(test)]
mod registration_tests;

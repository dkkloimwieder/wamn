use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use tokio_postgres::{GenericClient, Transaction};
use wamn_schema_control::{
    AppliedPackage, MigrationOutcome, MigrationSource, PackageDirectory, RecordedMigration,
};

pub const LOCK_PACKAGE_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.package.lineage:' || $1 || ':' || $2, 0))";
/// Record completed SQL in the same transaction as its effects.
///
/// The first fingerprint remains the baseline. A matching fingerprint is a
/// record match, not evidence that the live database is unchanged. The source
/// artifact reconstructs this schema even when its application stops running.
pub(super) async fn record_package_schema(
    tx: &Transaction<'_>,
    env_key: &str,
    coordinate: &wamn_catalog::PackageCoordinate,
    artifact_digest: Option<&str>,
    physical_schema_fingerprint: &str,
) -> anyhow::Result<bool> {
    let package_id = coordinate.package_id();
    Ok(tx
        .execute(
            "INSERT INTO catalog.packages \
         (env_key, package_id, package_version, artifact_digest, physical_schema_fingerprint) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (env_key, package_id) DO UPDATE \
         SET package_version = EXCLUDED.package_version, \
             artifact_digest = EXCLUDED.artifact_digest, \
             physical_schema_fingerprint = EXCLUDED.physical_schema_fingerprint \
         WHERE catalog.packages.physical_schema_fingerprint IS DISTINCT FROM \
               EXCLUDED.physical_schema_fingerprint",
            &[
                &env_key,
                &package_id,
                &coordinate.package_version(),
                &artifact_digest,
                &physical_schema_fingerprint,
            ],
        )
        .await
        .context("record completed package schema")?
        != 0)
}

pub async fn load_applied_package(
    tx: &Transaction<'_>,
    env_key: &str,
    package_id: &str,
) -> anyhow::Result<Option<AppliedPackage>> {
    read_package(
        tx,
        env_key,
        package_id,
        &wamn_schema_control::sql::select_package_sql(),
    )
    .await
}

/// Observe installed SQL in a read-only transaction without locking a package row.
pub async fn read_applied_package(
    tx: &Transaction<'_>,
    env_key: &str,
    package_id: &str,
) -> anyhow::Result<Option<AppliedPackage>> {
    read_package(
        tx,
        env_key,
        package_id,
        wamn_schema_control::sql::select_package_snapshot_sql(),
    )
    .await
}

async fn read_package(
    tx: &Transaction<'_>,
    env_key: &str,
    package_id: &str,
    query: &str,
) -> anyhow::Result<Option<AppliedPackage>> {
    let Some(package) = tx
        .query_opt(query, &[&env_key, &package_id])
        .await
        .context("read installed package schema")?
    else {
        return Ok(None);
    };
    let migrations = read_package_migrations(tx, env_key, package_id).await?;
    Ok(Some(AppliedPackage {
        coordinate: wamn_catalog::PackageCoordinate::new(package_id, package.get::<_, &str>(1))
            .expect("stored package coordinates passed database checks"),
        artifact_digest: package.get(0),
        baseline_fingerprint: package.get(3),
        physical_schema_fingerprint: package.get(2),
        migrations,
    }))
}

/// Read committed files even when the first package installation is unfinished.
pub async fn read_package_migrations(
    tx: &impl GenericClient,
    env_key: &str,
    package_id: &str,
) -> anyhow::Result<Vec<RecordedMigration>> {
    Ok(tx
        .query(
            wamn_schema_control::sql::select_package_migrations_sql(),
            &[&env_key, &package_id],
        )
        .await
        .context("read immutable package migration prefix")?
        .into_iter()
        .map(|row| RecordedMigration {
            ordinal: u32::try_from(row.get::<_, i32>(0))
                .expect("package migration ordinals are positive integers"),
            relative_path: row.get(1),
            sha256: row.get(2),
            outcome: match row.get::<_, &str>(3) {
                "applied" => MigrationOutcome::Applied,
                "started" => MigrationOutcome::Started,
                value => unreachable!("database constrains migration outcome: {value}"),
            },
        })
        .collect())
}

/// Derive installed SQL identity from this database's project baseline and current package records.
pub async fn installed_environment_fingerprint(
    tx: &impl GenericClient,
    env_key: &str,
) -> anyhow::Result<String> {
    let project: String = tx
        .query_one(
            "SELECT sha256 FROM platform.schema_baseline WHERE singleton",
            &[],
        )
        .await
        .context("read the installed project baseline")?
        .get(0);
    let rows = tx.query(
        "SELECT package_id, physical_schema_fingerprint FROM catalog.packages WHERE env_key = $1", &[&env_key]
    ).await.context("read installed package fingerprints")?;
    let packages = rows
        .iter()
        .map(|row| (row.get::<_, &str>(0), row.get::<_, &str>(1)))
        .collect();
    Ok(environment_fingerprint(&project, &packages))
}

/// Derive an environment recipe identity from its project and package SQL records.
pub fn environment_fingerprint(
    project: &str,
    packages: &std::collections::BTreeMap<&str, &str>,
) -> String {
    wamn_execution_contract::canonical_json_sha256(&serde_json::json!({
        "project": project,
        "packages": packages,
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

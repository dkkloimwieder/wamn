use std::path::Path;

use anyhow::{Context as _, ensure};
use tokio_postgres::Transaction;
use wamn_schema_control::{
    AppliedPackage, MigrationSource, PackageDirectory, PackageMigrationError, RecordedMigration,
    plan_package_registration,
};

use super::error::{ApplyPackageError, ApplyPackageErrorKind};

pub const LOCK_PACKAGE_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.package.lineage:' || $1 || ':' || $2, 0))";
const SELECT_PACKAGE_SQL: &str = "\
SELECT manifest_sha256, predecessor_version FROM catalog.packages \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
 FOR UPDATE";
const SELECT_MIGRATIONS_SQL: &str = "\
SELECT ordinal, relative_path, sha256 FROM catalog.package_migrations \
 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
 ORDER BY ordinal";
pub const SELECT_CURRENT_PACKAGE_VERSION_SQL: &str = "\
SELECT package.package_version FROM catalog.packages AS package \
 WHERE package.tenant_id = $1 AND package.package_id = $2 \
   AND NOT EXISTS (\
       SELECT 1 FROM catalog.packages AS successor \
        WHERE successor.tenant_id = package.tenant_id \
          AND successor.package_id = package.package_id \
          AND successor.predecessor_version = package.package_version\
   )";

/// Register a package while retaining its lineage lock through the caller's transaction.
pub async fn register_package(
    tx: &Transaction<'_>,
    tenant: &str,
    coordinate: &wamn_catalog::PackageCoordinate,
    manifest_sha256: &str,
    predecessor_version: Option<&str>,
) -> anyhow::Result<bool> {
    let package_id = coordinate.package_id();
    tx.query_one(LOCK_PACKAGE_SQL, &[&tenant, &package_id])
        .await
        .context("lock package lineage before registration")?;
    let recorded = tx
        .query_opt(
            SELECT_PACKAGE_SQL,
            &[&tenant, &package_id, &coordinate.package_version()],
        )
        .await
        .context("read package coordinate before registration")?;
    let recorded = recorded.map(|row| (row.get::<_, String>(0), row.get::<_, Option<String>>(1)));
    let current = current_package_version(tx, tenant, package_id).await?;
    let insert = plan_package_registration(
        coordinate,
        manifest_sha256,
        predecessor_version,
        recorded
            .as_ref()
            .map(|(hash, predecessor)| (hash.as_str(), predecessor.as_deref())),
        current.as_deref(),
    )?;
    if insert {
        tx.execute(
            "INSERT INTO catalog.packages \
             (tenant_id, package_id, package_version, manifest_sha256, predecessor_version) \
             VALUES ($1, $2, $3, $4, $5)",
            &[
                &tenant,
                &package_id,
                &coordinate.package_version(),
                &manifest_sha256,
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
    tenant: &str,
    package_id: &str,
) -> anyhow::Result<Option<String>> {
    tx.query_opt(SELECT_CURRENT_PACKAGE_VERSION_SQL, &[&tenant, &package_id])
        .await
        .context("read current package-family leaf")
        .map(|row| row.map(|row| row.get(0)))
}

pub(super) fn predecessor_not_current_error(
    coordinate: &str,
    declared_version: Option<&str>,
    current_version: &str,
) -> ApplyPackageError {
    ApplyPackageError {
        kind: ApplyPackageErrorKind::PredecessorNotCurrent,
        coordinate: coordinate.to_owned(),
        predecessor_version: declared_version.map(str::to_owned),
        current_version: Some(current_version.to_owned()),
        path: None,
        schema: None,
        relation: None,
        definition_kind: None,
        definition: None,
        owner_package: None,
        detail: "declare the current installed package version as predecessor_version".into(),
        source: None,
    }
}

pub(super) fn predecessor_prefix_error(
    coordinate: &str,
    predecessor_version: &str,
    source: Option<PackageMigrationError>,
    fallback_path: Option<&str>,
) -> ApplyPackageError {
    let path = source
        .as_ref()
        .and_then(PackageMigrationError::path)
        .or(fallback_path)
        .map(str::to_owned);
    let detail = source.as_ref().map_or_else(
        || {
            "declared predecessor is not applied; apply that exact predecessor before upgrading"
                .to_owned()
        },
        |source| format!("declared predecessor does not match the cumulative prefix: {source}"),
    );
    ApplyPackageError {
        kind: ApplyPackageErrorKind::PredecessorPrefixMismatch,
        coordinate: coordinate.to_owned(),
        predecessor_version: Some(predecessor_version.to_owned()),
        current_version: None,
        path,
        schema: None,
        relation: None,
        definition_kind: None,
        definition: None,
        owner_package: None,
        detail,
        source: source.map(|source| Box::new(source) as Box<dyn std::error::Error + Send + Sync>),
    }
}

pub async fn load_applied_package(
    tx: &Transaction<'_>,
    tenant: &str,
    package_id: &str,
    package_version: &str,
) -> anyhow::Result<Option<AppliedPackage>> {
    let Some(package) = tx
        .query_opt(
            SELECT_PACKAGE_SQL,
            &[&tenant, &package_id, &package_version],
        )
        .await
        .context("read immutable package root")?
    else {
        return Ok(None);
    };
    let migrations = tx
        .query(
            SELECT_MIGRATIONS_SQL,
            &[&tenant, &package_id, &package_version],
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
        manifest_sha256: package.get(0),
        migrations,
    }))
}

pub fn read_package_directory(root: &Path) -> anyhow::Result<PackageDirectory> {
    let manifest_path = root.join("wamn.json");
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let migrations_path = root.join("migrations");
    let entries = std::fs::read_dir(&migrations_path)
        .with_context(|| format!("read {}", migrations_path.display()))?;
    let mut migrations = Vec::new();
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
    Ok(PackageDirectory {
        manifest_bytes,
        migrations,
    })
}

#[cfg(test)]
mod registration_tests;

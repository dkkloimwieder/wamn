use sqlx::Row as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;

use wamn_schema_control::{AppliedPackage, MigrationSource, PackageDirectory};

pub const LOCK_PACKAGE_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.package.lineage:' || $1 || ':' || $2, 0))";

pub(super) async fn record_package_schema(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &str,
    coordinate: &wamn_catalog::PackageCoordinate,
    artifact_digest: Option<&str>,
) -> anyhow::Result<bool> {
    Ok(sqlx::query("INSERT INTO catalog.packages (project_env_key, package_id, package_version, artifact_digest) \
        VALUES ($1,$2,$3,$4) ON CONFLICT (project_env_key,package_id) DO UPDATE \
        SET package_version=EXCLUDED.package_version, artifact_digest=EXCLUDED.artifact_digest \
        WHERE ROW(catalog.packages.package_version, catalog.packages.artifact_digest) \
        IS DISTINCT FROM ROW(EXCLUDED.package_version, EXCLUDED.artifact_digest)")
        .bind(project_env_key).bind(coordinate.package_id()).bind(coordinate.package_version()).bind(artifact_digest)
        .execute(&mut **tx).await?.rows_affected() != 0)
}

pub async fn load_applied_package(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &str,
    package_id: &str,
) -> anyhow::Result<Option<AppliedPackage>> {
    read_package(
        tx,
        project_env_key,
        package_id,
        &wamn_schema_control::sql::select_package_sql(),
    )
    .await
}

pub async fn read_applied_package(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &str,
    package_id: &str,
) -> anyhow::Result<Option<AppliedPackage>> {
    read_package(
        tx,
        project_env_key,
        package_id,
        wamn_schema_control::sql::select_package_snapshot_sql(),
    )
    .await
}

async fn read_package(
    tx: &mut sqlx::PgConnection,
    project_env_key: &str,
    package_id: &str,
    query: &str,
) -> anyhow::Result<Option<AppliedPackage>> {
    let Some(row) = sqlx::query(sqlx::AssertSqlSafe(query.to_owned()))
        .bind(project_env_key)
        .bind(package_id)
        .fetch_optional(tx)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(AppliedPackage {
        coordinate: wamn_catalog::PackageCoordinate::new(package_id, row.get::<&str, _>(1))?,
        artifact_digest: row.get(0),
    }))
}

/// Read SQLx's immutable applied versions and checksums for one package.
pub async fn read_package_migrations(
    connection: &mut sqlx::PgConnection,
    project_env_key: &str,
    package_id: &str,
) -> anyhow::Result<Vec<sqlx::migrate::AppliedMigration>> {
    use sqlx::migrate::Migrate as _;
    let table = wamn_control_provision::migration::history_table(project_env_key, package_id);
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(&table)
        .fetch_one(&mut *connection)
        .await?;
    if !exists {
        return Ok(Vec::new());
    }
    Ok(connection.list_applied_migrations(&table).await?)
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
    let manifest_path = wamn_compiler::package_manifest_path(root);
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    read_migrations(&migration_root(root), manifest_bytes, artifact_digest)
}

pub(super) fn migration_root(root: &Path) -> PathBuf {
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

#[cfg(test)]
mod directory_tests;

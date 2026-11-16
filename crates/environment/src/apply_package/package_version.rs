use sqlx::Row as _;
use wamn_catalog::package_schema::AppliedPackage;

pub(super) async fn record_package_schema(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    coordinate: &wamn_catalog::PackageCoordinate,
    artifact_digest: Option<&wamn_digest::Digest>,
) -> anyhow::Result<bool> {
    Ok(sqlx::query("INSERT INTO catalog.packages (package_id, package_version, artifact_digest) \
        VALUES ($1,$2,$3) ON CONFLICT (package_id) DO UPDATE \
        SET package_version=EXCLUDED.package_version, artifact_digest=EXCLUDED.artifact_digest \
        WHERE ROW(catalog.packages.package_version, catalog.packages.artifact_digest) \
        IS DISTINCT FROM ROW(EXCLUDED.package_version, EXCLUDED.artifact_digest)")
        .bind(coordinate.package_id()).bind(coordinate.package_version()).bind(artifact_digest)
        .execute(&mut **tx).await?.rows_affected() != 0)
}

pub const LOCK_PACKAGE_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.package.lineage:' || $1 || ':' || $2, 0))";

pub async fn load_applied_package(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    package_id: &wamn_catalog::PackageName,
) -> anyhow::Result<Option<AppliedPackage>> {
    read_package(
        tx,
        package_id,
        "SELECT artifact_digest, package_version FROM catalog.packages WHERE package_id=$1 FOR UPDATE",
    )
    .await
}

pub async fn read_applied_package(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    package_id: &wamn_catalog::PackageName,
) -> anyhow::Result<Option<AppliedPackage>> {
    read_package(
        tx,
        package_id,
        "SELECT artifact_digest, package_version FROM catalog.packages WHERE package_id=$1",
    )
    .await
}

async fn read_package(
    tx: &mut sqlx::PgConnection,
    package_id: &wamn_catalog::PackageName,
    query: &str,
) -> anyhow::Result<Option<AppliedPackage>> {
    let Some(row) = sqlx::query(sqlx::AssertSqlSafe(query.to_owned()))
        .bind(package_id)
        .fetch_optional(tx)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(AppliedPackage {
        coordinate: wamn_catalog::PackageCoordinate::new(
            package_id.clone(),
            row.try_get::<wamn_catalog::package_version::PackageVersion, _>(1)?,
        )?,
        artifact_digest: row.get(0),
    }))
}

/// Read SQLx's immutable applied versions and checksums for one package.
pub async fn read_package_migrations(
    connection: &mut sqlx::PgConnection,
    project_env_key: &wamn_registry::ProjectEnvKey,
    package_id: &wamn_catalog::PackageName,
) -> anyhow::Result<Vec<sqlx::migrate::AppliedMigration>> {
    use sqlx::migrate::Migrate as _;
    let table = wamn_provision::migration::history_table(project_env_key, package_id);
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(&table)
        .fetch_one(&mut *connection)
        .await?;
    if !exists {
        return Ok(Vec::new());
    }
    Ok(connection.list_applied_migrations(&table).await?)
}

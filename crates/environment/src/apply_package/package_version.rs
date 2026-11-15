use sqlx::Row as _;
use wamn_catalog::package_schema::AppliedPackage;

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

pub const LOCK_PACKAGE_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.package.lineage:' || $1 || ':' || $2, 0))";

pub async fn load_applied_package(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &str,
    package_id: &str,
) -> anyhow::Result<Option<AppliedPackage>> {
    read_package(
        tx,
        project_env_key,
        package_id,
        "SELECT artifact_digest, package_version FROM catalog.packages WHERE project_env_key=$1 AND package_id=$2 FOR UPDATE",
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
        "SELECT artifact_digest, package_version FROM catalog.packages WHERE project_env_key=$1 AND package_id=$2",
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

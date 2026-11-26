use anyhow::Context as _;
use sqlx::Row as _;
use wamn_package::package_schema::AppliedPackage;

pub(super) async fn record_package_schema(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    coordinate: &wamn_package::PackageCoordinate,
    artifact_digest: Option<&wamn_digest::Digest>,
) -> anyhow::Result<bool> {
    Ok(sqlx::query(
        "INSERT INTO catalog.package (package_id, package_version, artifact_digest) \
        VALUES ($1,$2,$3) ON CONFLICT (package_id) DO UPDATE \
        SET package_version=EXCLUDED.package_version, artifact_digest=EXCLUDED.artifact_digest \
        WHERE ROW(catalog.package.package_version, catalog.package.artifact_digest) \
        IS DISTINCT FROM ROW(EXCLUDED.package_version, EXCLUDED.artifact_digest)",
    )
    .bind(coordinate.package_id().as_str())
    .bind(coordinate.package_version().as_str())
    .bind(artifact_digest.map(ToString::to_string))
    .execute(&mut **tx)
    .await?
    .rows_affected()
        != 0)
}

pub const LOCK_PACKAGE_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.package.lineage:' || $1 || ':' || $2, 0))";

pub async fn load_applied_package(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    package_id: &wamn_package::PackageName,
) -> anyhow::Result<Option<AppliedPackage>> {
    read_package(
        tx,
        package_id,
        "SELECT artifact_digest, package_version FROM catalog.package WHERE package_id=$1 FOR UPDATE",
    )
    .await
}

pub async fn read_applied_package(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    package_id: &wamn_package::PackageName,
) -> anyhow::Result<Option<AppliedPackage>> {
    read_package(
        tx,
        package_id,
        "SELECT artifact_digest, package_version FROM catalog.package WHERE package_id=$1",
    )
    .await
}

async fn read_package(
    tx: &mut sqlx::PgConnection,
    package_id: &wamn_package::PackageName,
    query: &str,
) -> anyhow::Result<Option<AppliedPackage>> {
    let Some(row) = sqlx::query(sqlx::AssertSqlSafe(query.to_owned()))
        .bind(package_id.as_str())
        .fetch_optional(tx)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(AppliedPackage {
        coordinate: wamn_package::PackageCoordinate::new(
            package_id.clone(),
            row.try_get::<String, _>(1)?
                .try_into()
                .context("decode package_version")?,
        )?,
        artifact_digest: row
            .try_get::<Option<&str>, _>(0)?
            .map(str::parse)
            .transpose()?,
    }))
}

/// Read SQLx's immutable applied versions and checksums for one package.
pub async fn read_package_migrations(
    connection: &mut sqlx::PgConnection,
    project_env_key: &wamn_registry::ProjectEnvKey,
    package_id: &wamn_package::PackageName,
) -> anyhow::Result<Vec<sqlx::migrate::AppliedMigration>> {
    use sqlx::migrate::Migrate as _;
    let table = crate::project_migration::history_table(project_env_key, package_id);
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(&table)
        .fetch_one(&mut *connection)
        .await?;
    if !exists {
        return Ok(Vec::new());
    }
    Ok(connection.list_applied_migrations(&table).await?)
}

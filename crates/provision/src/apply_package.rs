//! Initialize or explicitly advance package SQL from verified files.

use std::collections::BTreeSet;

use anyhow::{Context as _, ensure};

use wamn_component_policy::PackageManifest;
use wamn_package::package_schema::{PackageDirectory, package_metadata};
use wamn_postgres::{PlatformComponent, bind_platform_principal};

mod entity_maps;
mod error;
mod migration;
mod operation_grants;
mod package_version;

pub use error::{
    APPLY_PACKAGE_REFUSAL, ApplyPackageError, ApplyPackageErrorType,
    PACKAGE_ARTIFACT_REQUIRED_REFUSAL,
};
pub use package_version::{
    LOCK_PACKAGE_SQL, load_applied_package, read_applied_package, read_package_migrations,
};
pub use wamn_environment::apply_package::{ApplyOutcome, ApplyPackageRequest, PackageRoot};

pub const LOCK_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.package.data-access:' || current_database(), 0))";

/// Package bytes and migration policy prepared before database mutation.
#[derive(Debug)]
pub(crate) struct PreparedPackage {
    pub directory: PackageDirectory,
    pub manifest: PackageManifest,
    migrations: sqlx::migrate::Migrator,
}

/// Validate one package directory before opening an application transaction.
async fn prepare_package(
    directory: PackageDirectory,
    migration_root: &std::path::Path,
) -> anyhow::Result<PreparedPackage> {
    package_metadata::<wamn_component_policy::ComponentDescriptor>(&directory)
        .context("validate package directory before database work")?;
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes)
        .context("parse strict package manifest for definition ownership")?;
    let migrations = wamn_postgres::build_database::migrator(migration_root).await?;
    Ok(PreparedPackage {
        directory,
        manifest,
        migrations,
    })
}

/// Apply an absent package or advance its installed SQL with append-only migrations.
pub async fn apply_package(
    request: ApplyPackageRequest,
    directory: PackageDirectory,
    migration_root: &std::path::Path,
) -> anyhow::Result<ApplyOutcome> {
    let package = prepare_package(directory, migration_root).await?;
    if package.directory.artifact_digest.is_none() {
        return Err(ApplyPackageError::artifact_required(&format!(
            "{}@{}",
            package.manifest.package.id, package.manifest.package.version
        ))
        .into());
    }
    let mut client = wamn_postgres::connect(&request.database_url)
        .await
        .context("connect to project environment")?;

    let result = apply(
        &mut client,
        &request.migration_database_url,
        &request.database_url,
        &request.project_env_key(),
        package,
    )
    .await;
    drop(client);
    result
}

async fn apply(
    client: &mut sqlx::PgConnection,
    migration_input: &str,
    catalog_input: &str,
    project_env_key: &wamn_registry::ProjectEnvKey,
    package: PreparedPackage,
) -> anyhow::Result<ApplyOutcome> {
    let plan = package_metadata::<wamn_component_policy::ComponentDescriptor>(&package.directory)?;
    let package_id = plan.coordinate.package_id().to_owned();
    let package_version = plan.coordinate.package_version().to_owned();
    let coordinate = format!("{package_id}@{package_version}");
    let migration_options = wamn_postgres::connect_options(migration_input).await?;
    let catalog_options = wamn_postgres::connect_options(catalog_input).await?;
    ensure!(
        migration_options.get_host() == catalog_options.get_host()
            && migration_options.get_port() == catalog_options.get_port()
            && migration_options.get_database() == catalog_options.get_database(),
        "migration and catalog connections name different database targets"
    );
    let mut executor = wamn_postgres::connect(migration_options)
        .await
        .context("connect as the migration login")?;
    let login = wamn_postgres::project_migration::require_login(&mut executor).await?;
    let catalog_database: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&mut *client)
        .await?;
    let migration_database: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&mut executor)
        .await?;
    ensure!(
        catalog_database == migration_database,
        "migration and catalog connections name different databases"
    );
    // The executing backend owns exclusion, including while a cancelled SQL statement finishes.
    wamn_postgres::migrations::lock_session(&mut executor, LOCK_SQL, &[]).await?;
    wamn_postgres::migrations::lock_session(
        &mut executor,
        LOCK_PACKAGE_SQL,
        &[project_env_key.as_str(), package_id.as_str()],
    )
    .await?;
    wamn_postgres::migrations::lock_session(
        &mut executor,
        wamn_postgres::audit_retention::AUDIT_RETENTION_LOCK_SQL,
        &[],
    )
    .await?;
    let mut tx = migration::begin(client, project_env_key).await?;
    let artifact_digest = Some(
        package
            .directory
            .artifact_digest
            .as_ref()
            .ok_or_else(|| ApplyPackageError::artifact_required(&coordinate))?,
    );
    let applied = load_applied_package(&mut tx, &package_id).await?;
    let installed_migrations =
        read_package_migrations(&mut tx, project_env_key, &package_id).await?;
    validate_append(&package.migrations, &installed_migrations)?;
    tx.rollback().await?;
    let mut tx = migration::begin(client, project_env_key).await?;
    let mut changed = false;
    if applied.is_none() {
        changed |=
            wamn_postgres::release_requirements::install(&mut tx, &package.manifest).await? != 0;
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_postgres::project_migration::grants_sql(&login),
    ))
    .execute(&mut *tx)
    .await?;
    let table = wamn_postgres::project_migration::history_table(project_env_key, &package_id);
    wamn_postgres::project_migration::prepare_history(&mut tx, &table, &login).await?;
    ensure_model_schemas(&mut tx, &plan, &login).await?;
    tx.commit().await?;
    let migrations_applied =
        wamn_postgres::migrations::run(&mut executor, package.migrations, table).await?;
    let mut tx = migration::begin(client, project_env_key).await?;
    entity_maps::reconcile_entity_maps(&mut tx, &plan, &package.manifest).await?;
    changed |=
        package_version::record_package_schema(&mut tx, &plan.coordinate, artifact_digest).await?;
    if applied.is_none() {
        changed |= operation_grants::ensure_admin_role(&mut tx, project_env_key).await?;
    }
    tx.commit().await?;
    drop(executor);
    Ok(ApplyOutcome {
        package_id,
        package_version,
        migrations_applied,
        changed: changed || migrations_applied != 0,
    })
}

fn validate_append(
    migrations: &sqlx::migrate::Migrator,
    installed: &[sqlx::migrate::AppliedMigration],
) -> anyhow::Result<()> {
    let sources = migrations
        .iter()
        .filter(|file| file.migration_type.is_up_migration());
    for row in installed {
        let source = sources
            .clone()
            .find(|file| file.version == row.version)
            .ok_or(sqlx::migrate::MigrateError::VersionMissing(row.version))?;
        ensure!(
            source.checksum == row.checksum,
            sqlx::migrate::MigrateError::VersionMismatch(row.version)
        );
    }
    if let Some(highest) = installed.iter().map(|row| row.version).max() {
        for source in sources.filter(|file| file.version <= highest) {
            ensure!(
                installed.iter().any(|row| row.version == source.version),
                "new migration version {} must exceed installed version {highest}",
                source.version
            );
        }
    }
    Ok(())
}

/// Bind `wamn:apply-package` as the actor and the operation of the
/// transaction, so its writes, including the admin role, record that
/// component.
pub(crate) async fn bind_apply_package_principal(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> anyhow::Result<()> {
    bind_platform_principal(tx, PlatformComponent::ApplyPackage)
        .await
        .context("bind wamn:apply-package as the transaction actor and operation")
}

async fn ensure_model_schemas(
    connection: &mut sqlx::PgConnection,
    plan: &wamn_package::package_schema::PackageMetadata,
    login: &wamn_role::LoginName,
) -> anyhow::Result<()> {
    let schemas = plan
        .models
        .iter()
        .map(|model| model.schema.as_str())
        .chain(
            plan.cdc_excluded_relations
                .iter()
                .map(|relation| relation.schema.as_str()),
        )
        .collect::<BTreeSet<_>>();
    for schema in schemas {
        let schema = wamn_postgres::schema::BareSchemaName::new(schema)?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE SCHEMA IF NOT EXISTS {} AUTHORIZATION {}",
            schema.quoted(),
            postgres_protocol::escape::escape_identifier(login.as_str())
        )))
        .execute(&mut *connection)
        .await?;
        wamn_postgres::project_migration::grant_model_schema_access(
            connection,
            login,
            schema.as_str(),
        )
        .await?;
    }
    Ok(())
}

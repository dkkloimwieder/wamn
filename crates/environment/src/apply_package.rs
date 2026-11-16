//! Initialize or explicitly advance package SQL from verified files.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Context as _, ensure};

use wamn_catalog::package_schema::{PackageDirectory, package_metadata};
use wamn_compiler::manifest::PackageManifest;
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql};

mod entity_maps;
mod error;
mod migration;
mod operation_grants;
mod package_version;
#[cfg(test)]
mod tests;

pub use error::{
    APPLY_PACKAGE_REFUSAL, ApplyPackageError, ApplyPackageErrorType,
    PACKAGE_ARTIFACT_REQUIRED_REFUSAL,
};
pub use package_version::{
    LOCK_PACKAGE_SQL, load_applied_package, read_applied_package, read_package_migrations,
};
pub use wamn_publish::package_source::{PackageRoot, read_package_directory, read_package_root};

pub const LOCK_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.package.data-access:' || current_database(), 0))";

/// Inputs of one exact package application.
#[derive(Debug)]
pub struct ApplyPackageRequest {
    /// Component repository configuration for digest-addressed binary reads.
    pub component_source:
        Option<wamn_component_artifact::component_artifact_source::ComponentArtifactSourceConfig>,

    /// Package root containing strict wamn.json and migrations/, with the
    /// digest of the verified artifact it was opened from.
    pub package: PackageRoot,

    /// Publisher for auth facts after the package transaction commits.
    pub changes: wamn_broker::Publisher,

    /// Owner connection to the target project-environment database.
    pub database_url: String,

    /// Direct restricted migration login; never the catalog owner's credential.
    pub migration_database_url: String,

    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
    pub environment: wamn_registry::EnvName,
}

impl ApplyPackageRequest {
    /// The composed environment key derived from the environment coordinate.
    pub fn project_env_key(&self) -> wamn_registry::ProjectEnvKey {
        wamn_registry::project_env_key(&self.org, &self.project, &self.environment)
    }
}

/// Result of one exact package application.
#[derive(Debug)]
pub struct ApplyOutcome {
    /// Package id of the applied coordinate.
    pub package_id: wamn_catalog::PackageName,

    /// Package version of the applied coordinate.
    pub package_version: wamn_catalog::package_version::PackageVersion,

    /// Number of pending migrations this application ran.
    pub migrations_applied: usize,

    /// Whether this application changed the database.
    pub changed: bool,

    pub drops: wamn_broker::auth_changes::AuthDrops,
}

/// Package bytes and migration policy prepared before database mutation.
#[derive(Debug)]
pub(crate) struct PreparedPackage {
    pub component_source:
        Option<wamn_component_artifact::component_artifact_source::ComponentArtifactSourceConfig>,
    pub root: PathBuf,
    pub directory: PackageDirectory,
    pub manifest: PackageManifest,
    migrations: sqlx::migrate::Migrator,
}

/// Validate one package directory before opening an application transaction.
pub(crate) async fn prepare_package(root: &PackageRoot) -> anyhow::Result<PreparedPackage> {
    let directory = read_package_root(root)?;
    package_metadata(&directory).context("validate package directory before database work")?;
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes)
        .context("parse strict package manifest for definition ownership")?;
    let migration_root = wamn_publish::package_source::migration_root(&root.path);
    let migrations = wamn_compiler::migrations::migrator(&migration_root).await?;
    Ok(PreparedPackage {
        component_source: None,
        root: root.path.clone(),
        directory,
        manifest,
        migrations,
    })
}

/// Initialize an absent package. Existing packages retain their installed SQL.
pub async fn apply_package(request: ApplyPackageRequest) -> anyhow::Result<ApplyOutcome> {
    apply_request(request, false).await
}

/// Explicitly advance package SQL without selecting or caching executable releases.
pub async fn apply_package_schema(request: ApplyPackageRequest) -> anyhow::Result<ApplyOutcome> {
    apply_request(request, true).await
}

async fn apply_request(
    request: ApplyPackageRequest,
    advance_schema: bool,
) -> anyhow::Result<ApplyOutcome> {
    let mut package = prepare_package(&request.package).await?;
    if package.directory.artifact_digest.is_none() {
        return Err(ApplyPackageError::artifact_required(&format!(
            "{}@{}",
            package.manifest.package.id, package.manifest.package.version
        ))
        .into());
    }
    package
        .component_source
        .clone_from(&request.component_source);
    let mut client = wamn_postgres::connect(&request.database_url)
        .await
        .context("connect to project environment")?;

    let result = apply(
        &mut client,
        &request.migration_database_url,
        &request.database_url,
        &request.project_env_key(),
        package,
        advance_schema,
    )
    .await;
    if let Ok(outcome) = &result {
        request.changes.publish(outcome.drops.clone()).await;
    }
    drop(client);
    result
}

async fn apply(
    client: &mut sqlx::PgConnection,
    migration_input: &str,
    catalog_input: &str,
    project_env_key: &wamn_registry::ProjectEnvKey,
    package: PreparedPackage,
    advance_schema: bool,
) -> anyhow::Result<ApplyOutcome> {
    let plan = package_metadata(&package.directory)?;
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
    let login =
        wamn_control_provision::migration::require_login(&mut executor, project_env_key).await?;
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
    crate::migrations::lock_session(&mut executor, LOCK_SQL, &[]).await?;
    crate::migrations::lock_session(
        &mut executor,
        LOCK_PACKAGE_SQL,
        &[project_env_key.as_str(), package_id.as_str()],
    )
    .await?;
    crate::migrations::lock_session(
        &mut executor,
        wamn_control_provision::audit_retention::AUDIT_RETENTION_LOCK_SQL,
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
    let applied = load_applied_package(&mut tx, project_env_key, &package_id).await?;
    if !advance_schema && let Some(installed) = &applied {
        wamn_catalog::package_schema::validate_package_schema_source(
            &plan.coordinate,
            artifact_digest,
            installed,
        )?;
        return Ok(ApplyOutcome {
            package_id,
            package_version,
            migrations_applied: 0,
            changed: false,
            drops: wamn_broker::auth_changes::AuthDrops::default(),
        });
    }
    if let Some(installed) = &applied {
        wamn_catalog::package_schema::validate_package_schema_source(
            &plan.coordinate,
            artifact_digest,
            installed,
        )?;
    }
    tx.rollback().await?;
    let mut tx = migration::begin(client, project_env_key).await?;
    let mut changed = false;
    if applied.is_none() {
        changed |= wamn_publish::connection_requirement::install(
            &mut tx,
            project_env_key,
            &package.root,
            &package.manifest,
            package.component_source.as_ref(),
        )
        .await?
            != 0;
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::migration::grants_sql(&login),
    ))
    .execute(&mut *tx)
    .await?;
    let table = wamn_control_provision::migration::history_table(project_env_key, &package_id);
    wamn_control_provision::migration::prepare_history(&mut tx, &table, &login).await?;
    ensure_model_schemas(&mut tx, &plan, &login).await?;
    tx.commit().await?;
    let migrations_applied =
        crate::migrations::run(&mut executor, package.migrations, table).await?;
    let mut tx = migration::begin(client, project_env_key).await?;
    entity_maps::reconcile_entity_maps(&mut tx, &plan, &package.manifest).await?;
    changed |= package_version::record_package_schema(
        &mut tx,
        project_env_key,
        &plan.coordinate,
        artifact_digest,
    )
    .await?;
    let mut drops = wamn_broker::auth_changes::AuthDrops::default();
    if applied.is_none() {
        changed |=
            operation_grants::ensure_admin_role(&mut tx, project_env_key, &mut drops).await?;
    }
    tx.commit().await?;
    drop(executor);
    Ok(ApplyOutcome {
        package_id,
        package_version,
        migrations_applied,
        changed: changed || migrations_applied != 0,
        drops,
    })
}

/// Bind `wamn:apply-package` as the actor and the operation of the
/// transaction, so its writes, including the admin role, record that
/// component.
pub(crate) async fn bind_apply_package_principal(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> anyhow::Result<()> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(bind_platform_principal_sql(
        PlatformComponent::ApplyPackage,
    )))
    .execute(&mut **tx)
    .await
    .map(drop)
    .context("bind wamn:apply-package as the transaction actor and operation")
}

async fn ensure_model_schemas(
    connection: &mut sqlx::PgConnection,
    plan: &wamn_catalog::package_schema::PackageMetadata,
    login: &wamn_identity_protocol::LoginName,
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
            "CREATE SCHEMA IF NOT EXISTS {} AUTHORIZATION wamn_db_owner",
            schema.quoted()
        )))
        .execute(&mut *connection)
        .await?;
        wamn_control_provision::migration::grant_model_schema_access(
            connection,
            login,
            schema.as_str(),
        )
        .await?;
    }
    Ok(())
}

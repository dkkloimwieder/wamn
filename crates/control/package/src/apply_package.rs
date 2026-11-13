//! Initialize or explicitly advance package SQL from verified files.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use tokio_postgres::{NoTls, Transaction};
use wamn_catalog::manifest::PackageManifest;
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql};
use wamn_schema_control::{PackageDirectory, plan_package_migrations};

use definition_ownership::{validate_definition_ownership_before_apply, validate_migration_policy};

mod adoption;
pub use adoption::adopt_package_schemas;
mod definition_ownership;
mod entity_maps;
mod error;
mod local_target;
mod migration;
pub use migration::{MigrationResolution, Resolution, resolution_package};
mod operation_grants;
mod package_version;
mod record_history;
mod roles;
#[cfg(test)]
mod tests;

pub use error::{
    APPLY_PACKAGE_REFUSAL, ApplyPackageError, ApplyPackageErrorType,
    BASE_DEFINITION_MUTATION_REFUSAL, DEFINITION_NOT_FOUND_REFUSAL,
    DEFINITION_OWNER_CONFLICT_REFUSAL, DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL,
    PACKAGE_ARTIFACT_REQUIRED_REFUSAL, RELATION_NOT_CLIENT_EXTENSIBLE_REFUSAL,
};
pub use local_target::{manifest_digest, record_manifest};
pub use package_version::{
    LOCK_PACKAGE_SQL, PackageRoot, environment_fingerprint, installed_environment_fingerprint,
    load_applied_package, read_applied_package, read_package_directory, read_package_migrations,
    read_package_root,
};

pub const LOCK_SQL: &str = "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.package.data-access:' || current_database(), 0))";

const ASSIGN_ENV_KEY_SQL: &str = "SELECT set_config('app.env_key', $1, true)";

/// Inputs of one exact package application.
#[derive(Debug)]
pub struct ApplyPackageRequest {
    /// Component repository configuration for digest-addressed binary reads.
    pub component_source:
        Option<wamn_runtime::component_artifact_source::ComponentArtifactSourceConfig>,

    /// Package root containing strict wamn.json and migrations/, with the
    /// digest of the verified artifact it was opened from.
    pub package: PackageRoot,

    /// Publisher for auth facts after the package transaction commits.
    pub changes: wamn_broker::Publisher,

    /// Owner connection to the target project-environment database.
    pub database_url: String,

    pub org: String,
    pub project: String,
    pub environment: String,
}

impl ApplyPackageRequest {
    /// The composed environment key derived from the environment coordinate.
    pub fn env_key(&self) -> String {
        wamn_control_registry::project_env_key(&self.org, &self.project, &self.environment)
    }
}

/// Read the existing component repository variable with the package registry's transport.
pub fn component_source_from_env(
    registry: Option<&crate::package_artifact::PackageRegistry>,
) -> anyhow::Result<Option<wamn_runtime::component_artifact_source::ComponentArtifactSourceConfig>>
{
    use wamn_runtime::component_artifact_source::{
        ComponentArtifactSourceConfig, OCI_CA_PATHS_ENV,
    };
    let base = match std::env::var("WAMN_COMPONENT_ARTIFACT_BASE") {
        Ok(base) => base,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(error) => return Err(error).context("read WAMN_COMPONENT_ARTIFACT_BASE"),
    };
    let mut source = ComponentArtifactSourceConfig::new(
        &base,
        registry.is_some_and(|registry| registry.insecure_registry),
        std::time::Duration::from_secs(30),
    )?;
    if let Some(registry) = registry {
        source = source
            .with_registry_auth_file(&registry.registry_auth_file)?
            .with_ca_paths(&registry.oci_ca_paths)?;
    } else {
        if let Some(file) = std::env::var_os("WAMN_REGISTRY_AUTH_FILE") {
            source = source.with_registry_auth_file(Path::new(&file))?;
        }
        if let Some(paths) = std::env::var_os(OCI_CA_PATHS_ENV) {
            source = source.with_ca_paths(
                &paths
                    .to_string_lossy()
                    .split(',')
                    .filter(|path| !path.is_empty())
                    .map(PathBuf::from)
                    .collect::<Vec<_>>(),
            )?;
        }
    }
    Ok(Some(source))
}

/// Result of one exact package application.
#[derive(Debug)]
pub struct ApplyOutcome {
    /// Package id of the applied coordinate.
    pub package_id: String,

    /// Package version of the applied coordinate.
    pub package_version: String,

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
        Option<wamn_runtime::component_artifact_source::ComponentArtifactSourceConfig>,
    pub root: PathBuf,
    pub directory: PackageDirectory,
    pub manifest: PackageManifest,
    policy: definition_ownership::MigrationPolicyPlan,
}

/// Validate one package directory before opening an application transaction.
pub(crate) fn prepare_package(root: &PackageRoot) -> anyhow::Result<PreparedPackage> {
    let directory = read_package_root(root)?;
    let presented = plan_package_migrations(&directory, None, &[])
        .context("validate package directory before database work")?;
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes)
        .context("parse strict package manifest for definition ownership")?;
    wamn_catalog::manifest::validate_operation_vocabulary(&manifest)
        .context("validate package manifest for registration projection")?;
    let policy = validate_migration_policy(&root.path, &directory, &presented)?;
    Ok(PreparedPackage {
        component_source: None,
        root: root.path.clone(),
        directory,
        manifest,
        policy,
    })
}

/// Validate the complete migration policy before reconstructing a database.
/// Refusals deferred for application ownership checks also prevent reconstruction.
pub fn validate_package_migration_policy(
    root: &Path,
    directory: &PackageDirectory,
) -> anyhow::Result<()> {
    let plan = plan_package_migrations(directory, None, &[])?;
    let policy = validate_migration_policy(root, directory, &plan).with_context(|| {
        format!(
            "package {}@{} migration policy",
            plan.coordinate.package_id(),
            plan.coordinate.package_version()
        )
    })?;
    if let Some(refusal) = policy.deferred {
        return Err(refusal.source).with_context(|| {
            format!(
                "package {}@{} migration policy in {}",
                plan.coordinate.package_id(),
                plan.coordinate.package_version(),
                refusal.relative_path
            )
        });
    }
    Ok(())
}

/// Initialize an absent package. Existing packages retain their installed SQL.
pub async fn apply_package(request: ApplyPackageRequest) -> anyhow::Result<ApplyOutcome> {
    apply_request(request, None, false, None).await
}

/// Explicitly advance package SQL without selecting or caching executable releases.
pub async fn apply_package_schema(
    request: ApplyPackageRequest,
    resolution: Option<&MigrationResolution>,
) -> anyhow::Result<ApplyOutcome> {
    apply_request(request, None, true, resolution).await
}

/// Initialize missing local SQL without advancing an installed schema from the watcher.
pub async fn apply_local_package(
    request: ApplyPackageRequest,
    environment: &str,
) -> anyhow::Result<ApplyOutcome> {
    require_local_package(request, environment, false, None).await
}

/// Explicitly apply local SQL without publishing or activating application code.
pub async fn apply_local_package_schema(
    request: ApplyPackageRequest,
    environment: &str,
    resolution: Option<&MigrationResolution>,
) -> anyhow::Result<ApplyOutcome> {
    require_local_package(request, environment, true, resolution).await
}

async fn require_local_package(
    request: ApplyPackageRequest,
    environment: &str,
    advance_schema: bool,
    resolution: Option<&MigrationResolution>,
) -> anyhow::Result<ApplyOutcome> {
    wamn_runtime::local_application::require_local_target(
        &request.database_url,
        &request.env_key(),
        environment,
    )
    .await?;
    apply_request(request, Some(environment), advance_schema, resolution).await
}

async fn apply_request(
    request: ApplyPackageRequest,
    local_environment: Option<&str>,
    advance_schema: bool,
    resolution: Option<&MigrationResolution>,
) -> anyhow::Result<ApplyOutcome> {
    ensure!(
        ![&request.org, &request.project, &request.environment]
            .iter()
            .any(|part| part.is_empty()),
        "org, project and environment must not be empty"
    );
    let mut package = prepare_package(&request.package)?;
    package
        .component_source
        .clone_from(&request.component_source);
    let (mut client, connection) = tokio_postgres::connect(&request.database_url, NoTls)
        .await
        .context("connect to project environment")?;
    let connection_task = tokio::spawn(connection);
    let result = apply(
        &mut client,
        &request.env_key(),
        &package,
        local_environment,
        advance_schema,
        resolution,
    )
    .await;
    if let Ok(outcome) = &result {
        request.changes.publish(outcome.drops.clone()).await;
    }
    drop(client);
    if result.is_err() {
        connection_task.abort();
    } else {
        connection_task
            .await
            .context("join package database connection")?
            .context("drive package database connection")?;
    }
    result
}

async fn apply(
    client: &mut tokio_postgres::Client,
    env_key: &str,
    package: &PreparedPackage,
    local_environment: Option<&str>,
    advance_schema: bool,
    resolution: Option<&MigrationResolution>,
) -> anyhow::Result<ApplyOutcome> {
    let presented = plan_package_migrations(&package.directory, None, &[])?;
    // Match the transaction lock keys, but retain exclusion across file commits.
    // apply_request closes this dedicated connection on every result.
    crate::migrations::lock_session(client, LOCK_SQL, &[]).await?;
    crate::migrations::lock_session(
        client,
        LOCK_PACKAGE_SQL,
        &[&env_key, &presented.coordinate.package_id()],
    )
    .await?;
    let tx = migration::begin(client, env_key).await?;
    let package_root = &package.root;
    let directory = &package.directory;
    let manifest = &package.manifest;
    let definition_ownership::MigrationPolicyPlan {
        mutations,
        deferred,
    } = &package.policy;
    let package_id = presented.coordinate.package_id().to_owned();
    let package_version = presented.coordinate.package_version().to_owned();
    let coordinate_label = format!("{package_id}@{package_version}");
    let coordinate_text = coordinate_label.as_str();
    let local_comment = match local_environment {
        Some(environment) => Some(
            wamn_runtime::local_application::read_local_target_comment(&tx, env_key, environment)
                .await?,
        ),
        None => None,
    };

    // A release apply takes only a verified artifact. wamn dev applies its
    // working tree and records no artifact digest.
    let artifact_digest = if local_comment.is_some() {
        None
    } else {
        Some(
            directory
                .artifact_digest
                .as_deref()
                .ok_or_else(|| ApplyPackageError::artifact_required(coordinate_text))?,
        )
    };
    let applied = load_applied_package(&tx, env_key, &package_id).await?;
    // Executable selection, reapply, and rollback never advance SQL, even if
    // the selected artifact carries older or edited migration inputs.
    if !advance_schema && let Some(installed) = &applied {
        wamn_schema_control::validate_package_schema_source(
            &presented.coordinate,
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
    let recorded = read_package_migrations(&tx, env_key, &package_id).await?;
    let plan = plan_package_migrations(directory, applied.as_ref(), &recorded)?;
    crate::migrations::validate_resolution(
        crate::migrations::ResolutionTarget::Package(&package_id),
        &plan.pending,
        &recorded,
        resolution,
    )?;
    let migration_changed = !plan.is_noop();
    if let Some(deferred) = deferred {
        let paths = plan
            .pending
            .iter()
            .map(|file| file.relative_path.as_str())
            .collect::<BTreeSet<_>>();
        let pending_mutations = mutations
            .iter()
            .filter(|mutation| paths.contains(mutation.relative_path.as_ref()))
            .collect::<Vec<_>>();
        validate_definition_ownership_before_apply(
            &tx,
            env_key,
            coordinate_text,
            &package_id,
            manifest,
            &pending_mutations,
        )
        .await?;
        let refusal = validate_migration_policy(package_root, directory, &plan)?
            .deferred
            .expect("the package retains its policy refusal")
            .source;
        return Err(refusal)
            .with_context(|| format!("validate {} before apply", deferred.relative_path));
    }

    // Reopen through the shared executor. No package mutation precedes its first transaction.
    tx.rollback()
        .await
        .context("finish package migration planning")?;
    let keys: [(&str, &(dyn tokio_postgres::types::ToSql + Sync)); 2] =
        [("env_key", &env_key), ("package_id", &package_id)];
    let table = crate::migrations::MigrationTable {
        name: "catalog.package_migrations",
        keys: &keys,
        recorded: &recorded,
        target: crate::migrations::ResolutionTarget::Package(&package_id),
    };
    let mut hooks = migration::PackageMigrationHooks {
        env_key,
        package,
        plan: &plan,
        coordinate: coordinate_text,
        initial: applied.is_none(),
        artifact_digest,
        local_comment,
        changed: migration_changed,
        drops: wamn_broker::auth_changes::AuthDrops::default(),
    };
    let executed = crate::migrations::execute_migrations(
        client,
        &plan.pending,
        &plan.statements,
        &table,
        resolution,
        &mut hooks,
    )
    .await?;
    Ok(ApplyOutcome {
        drops: hooks.drops,
        package_id,
        package_version,
        migrations_applied: executed,
        changed: hooks.changed,
    })
}

async fn installed_physical_fingerprint(
    tx: &Transaction<'_>,
    directory: &PackageDirectory,
    manifest: &PackageManifest,
) -> anyhow::Result<String> {
    use wamn_schema_control::physical_schema::{PhysicalSchema, render};
    let mut columns = std::collections::BTreeMap::new();
    for (id, model) in &manifest.models {
        if model.owner == manifest.package.id {
            columns.insert(
                id.clone(),
                record_history::read_relation_columns(tx, &model.schema, &model.table).await?,
            );
        }
    }
    let statements = render(manifest, &directory.migrations, &columns)?;
    Ok(PhysicalSchema {
        columns,
        statements,
    }
    .fingerprint())
}

/// Bind `wamn:apply-package` as the actor and the operation of the
/// transaction, so its writes, including the admin role, record that
/// component.
pub(crate) async fn bind_apply_package_principal(tx: &Transaction<'_>) -> anyhow::Result<()> {
    tx.batch_execute(&bind_platform_principal_sql(
        PlatformComponent::ApplyPackage,
    ))
    .await
    .context("bind wamn:apply-package as the transaction actor and operation")
}

async fn ensure_model_schemas(
    tx: &Transaction<'_>,
    plan: &wamn_schema_control::PackageMigrationPlan,
    manifest: &PackageManifest,
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
    let package = &manifest.package.id;
    tx.batch_execute(&wamn_control_provision::sql::ensure_package_owner_role_sql(
        package,
    ))
    .await
    .context("provision the package owner")?;
    tx.batch_execute(&wamn_control_provision::sql::package_owner_grants_sql(
        package,
    ))
    .await
    .context("grant the package history surface")?;
    let role = postgres_protocol::escape::escape_identifier(
        &wamn_control_provision::package_owner_role(package),
    );
    for schema in schemas {
        let schema = wamn_schema_control::BareSchemaName::new(schema)
            .context("validate manifest model schema for creation")?;
        tx.batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {} AUTHORIZATION {role}; GRANT USAGE, CREATE ON SCHEMA {} TO {role}", schema.quoted(), schema.quoted()))
            .await
            .with_context(|| format!("ensure package model schema {schema}"))?;
        let unowned: bool = tx.query_one(
            "SELECT NOT EXISTS (SELECT 1 FROM catalog.package_definition_owners WHERE schema_name = $1 AND definition_type = 'relation')",
            &[&schema.as_str()],
        ).await?.get(0);
        if unowned {
            tx.batch_execute(&format!("ALTER SCHEMA {} OWNER TO {role}", schema.quoted()))
                .await?;
        }
    }
    Ok(())
}

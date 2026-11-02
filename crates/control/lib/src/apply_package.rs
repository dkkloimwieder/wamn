//! Install one package baseline from immutable files.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use tokio_postgres::{NoTls, Transaction, error::SqlState};
use wamn_catalog::manifest::PackageManifest;
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql};
use wamn_schema_control::{
    AppliedPackage, PackageDirectory, RecordedMigration, SqlStatement, plan_package_migrations,
};

use definition_ownership::{
    reconcile_definition_ownership, validate_definition_ownership_before_apply,
    validate_migration_policy,
};
use entity_maps::reconcile_entity_maps;
use operation_grants::ensure_admin_role;
use package_version::current_package_version;
use record_history::{create_history_tables, reconcile_record_history_triggers};
use roles::assert_host_role;

mod definition_ownership;
mod entity_maps;
mod error;
mod local_target;
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
    PACKAGE_ARTIFACT_REQUIRED_REFUSAL, PACKAGE_VERSION_SEALED_REFUSAL,
    RELATION_NOT_CLIENT_EXTENSIBLE_REFUSAL,
};
pub use local_target::{applied_migration_drift, local_target_recreate_reason};
pub(crate) use local_target::{local_target_changes, manifest_digest, record_manifest};
pub use package_version::{
    LOCK_PACKAGE_SQL, PackageRoot, SELECT_CURRENT_PACKAGE_VERSION_SQL, load_applied_package,
    read_package_directory, read_package_root, register_package,
};

const CLAIM_ENV_KEY_SQL: &str = "SELECT set_config('app.env_key', $1, true)";

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
    pub changes: wamn_platform_identity::auth_changes::AuthChanges,

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

    pub(crate) drops: wamn_platform_identity::auth_changes::AuthDrops,
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
    prepare_directory(root, directory)
}

fn prepare_directory(
    root: &PackageRoot,
    directory: PackageDirectory,
) -> anyhow::Result<PreparedPackage> {
    let presented = plan_package_migrations(&directory, None)
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

/// Apply the immutable pending suffix from one package directory.
pub async fn apply_package(request: ApplyPackageRequest) -> anyhow::Result<ApplyOutcome> {
    apply_request(request, None).await
}

/// Apply one package directory to a local target that wamn dev created.
///
/// The target also takes a changed tree at an applied coordinate, and a
/// migration appended after release membership, while every applied migration
/// stays byte-identical. The target records no artifact digest: its
/// `catalog.packages` row holds NULL, and the database comment records the
/// digest of the current `wamn.json`.
pub async fn apply_local_package(
    request: ApplyPackageRequest,
    environment: &str,
) -> anyhow::Result<ApplyOutcome> {
    wamn_runtime::local_application::require_local_target(
        &request.database_url,
        &request.env_key(),
        environment,
    )
    .await?;
    apply_request(request, Some(environment)).await
}

async fn apply_request(
    request: ApplyPackageRequest,
    local_environment: Option<&str>,
) -> anyhow::Result<ApplyOutcome> {
    ensure!(
        ![&request.org, &request.project, &request.environment]
            .iter()
            .any(|part| part.is_empty()),
        "org, project and environment must not be empty"
    );
    let mut package = if local_environment.is_some() {
        prepare_directory(
            &request.package,
            package_version::read_source_package_directory(&request.package.path)?,
        )?
    } else {
        prepare_package(&request.package)?
    };
    package
        .component_source
        .clone_from(&request.component_source);
    let (mut client, connection) = tokio_postgres::connect(&request.database_url, NoTls)
        .await
        .context("connect to project environment")?;
    let connection_task = tokio::spawn(connection);
    let result = apply(&mut client, &request.env_key(), &package, local_environment).await;
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
) -> anyhow::Result<ApplyOutcome> {
    let presented = plan_package_migrations(&package.directory, None)?;
    let tx = client.transaction().await.context("begin package apply")?;
    tx.query_one(CLAIM_ENV_KEY_SQL, &[&env_key])
        .await
        .context("claim package tenant")?;
    bind_apply_package_principal(&tx).await?;
    // Keep the same lock order as reconciliation and release selection:
    // data access, package lineage, then the selected release head.
    tx.query_one(crate::reconcile_package_data_access::LOCK_SQL, &[])
        .await
        .context("lock data access before package application")?;
    tx.query_one(
        LOCK_PACKAGE_SQL,
        &[&env_key, &presented.coordinate.package_id()],
    )
    .await
    .context("lock package family")?;
    let outcome = apply_prepared(&tx, env_key, package, local_environment).await?;
    tx.commit().await.context("commit whole package suffix")?;
    Ok(outcome)
}

async fn apply_prepared(
    tx: &Transaction<'_>,
    env_key: &str,
    package: &PreparedPackage,
    local_environment: Option<&str>,
) -> anyhow::Result<ApplyOutcome> {
    let package_root = &package.root;
    let directory = &package.directory;
    let manifest = &package.manifest;
    let definition_ownership::MigrationPolicyPlan {
        mutations,
        deferred,
    } = &package.policy;
    let presented =
        plan_package_migrations(directory, None).context("validate package directory")?;
    let package_id = presented.coordinate.package_id().to_owned();
    let package_version = presented.coordinate.package_version().to_owned();
    let coordinate_label = format!("{package_id}@{package_version}");
    let coordinate_text = coordinate_label.as_str();
    let mut local_comment = match local_environment {
        Some(environment) => Some(local_target::lift_release_seal(tx, env_key, environment).await?),
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
                .clone()
                .ok_or_else(|| ApplyPackageError::artifact_required(coordinate_text))?,
        )
    };
    let applied = load_applied_package(tx, env_key, &package_id, &package_version).await?;
    let plan = if let Some(applied) = applied.as_ref() {
        let compared = if local_comment.is_some() {
            // A local target takes a changed tree at the same coordinate.
            // The planner still refuses an edited, removed, or reordered
            // applied migration.
            plan_package_migrations(
                directory,
                Some(&AppliedPackage {
                    artifact_digest: presented.artifact_digest.clone(),
                    ..applied.clone()
                }),
            )
        } else {
            plan_package_migrations(directory, Some(applied))
        };
        compared.context("compare package bytes with immutable records")?
    } else {
        wamn_schema_control::plan_package_registration(
            &presented.coordinate,
            presented.artifact_digest.as_deref(),
            presented.predecessor_version.as_deref(),
            None,
            current_package_version(tx, env_key, &package_id)
                .await?
                .as_deref(),
        )?;
        presented
    };
    let requirements_written = crate::connection_requirement::install(
        tx,
        env_key,
        package_root,
        manifest,
        package.component_source.as_ref(),
    )
    .await?;
    let applied_count = plan.pending.len();
    let migration_changed = !plan.is_noop();
    let pending_paths = plan
        .pending
        .iter()
        .map(|migration| migration.relative_path.as_str())
        .collect::<BTreeSet<_>>();
    let pending_mutations = mutations
        .iter()
        .filter(|planned| pending_paths.contains(planned.relative_path.as_ref()))
        .collect::<Vec<_>>();

    validate_definition_ownership_before_apply(
        tx,
        env_key,
        coordinate_text,
        &package_id,
        manifest,
        &pending_mutations,
    )
    .await?;
    if let Some(deferred) = deferred {
        let refusal = validate_migration_policy(package_root, directory, &plan)?
            .deferred
            .expect("the package retains its policy refusal")
            .source;
        return Err(refusal)
            .with_context(|| format!("validate {} before apply", deferred.relative_path));
    }

    // wamn-yk9l. The platform extensions go in FIRST, while this connection is
    // still the administrator: `CREATE EXTENSION` is refused to a package by
    // the migration policy and is not a privilege the package-owner role holds.
    // Idempotent, so it converges a database provisioned before the list
    // existed.
    tx.batch_execute(&wamn_control_provision::sql::install_platform_extensions_sql())
        .await
        .context("install the platform extensions")?;

    // Provision package authority before any authored DDL. The migration
    // runner selects the owning role after the policy refuses role changes.
    ensure_model_schemas(tx, &plan, manifest).await?;
    // catalog.packages is immutable, so a local target keeps the row it first
    // recorded there and records the current manifest in its comment.
    let registered_artifact_digest = match &applied {
        Some(applied) if local_comment.is_some() => applied.artifact_digest.as_deref(),
        _ => artifact_digest.as_deref(),
    };
    let package_inserted = register_package(
        tx,
        env_key,
        &plan.coordinate,
        registered_artifact_digest,
        plan.predecessor_version.as_deref(),
    )
    .await
    .context("register package before applying migrations")?;
    for statement in &plan.statements {
        // The planner carries exact package bytes as its parameter-free batch
        // statements; every host-authored record statement has binds.
        if statement.params.is_empty() {
            roles::execute_migration(tx, manifest, statement).await?;
        } else {
            assert_host_role(tx).await?;
            execute(tx, statement, coordinate_text).await?;
        }
    }
    assert_host_role(tx).await?;
    let ownership_changed = reconcile_definition_ownership(
        tx,
        env_key,
        coordinate_text,
        &package_id,
        manifest,
        &pending_mutations,
    )
    .await?;
    let history_changed = create_history_tables(tx, manifest).await?;
    reconcile_entity_maps(tx, &plan, manifest).await?;
    let triggers_changed = reconcile_record_history_triggers(tx, manifest).await?;
    let mut drops = wamn_platform_identity::auth_changes::AuthDrops::default();
    let admin_role_changed = ensure_admin_role(tx, env_key, &mut drops).await?;
    let comment_changed = match local_comment.as_mut() {
        Some(comment) => {
            local_target::record_manifest(
                tx,
                comment,
                coordinate_text,
                &local_target::manifest_digest(directory),
            )
            .await?
        }
        None => false,
    };
    Ok(ApplyOutcome {
        drops,
        package_id,
        package_version,
        migrations_applied: applied_count,
        changed: requirements_written != 0
            || package_inserted
            || migration_changed
            || ownership_changed
            || history_changed
            || triggers_changed
            || admin_role_changed
            || comment_changed,
    })
}

/// Reconcile mutable local configuration only after confirming the installed schema.
pub async fn reconcile_local_package_configuration(
    tx: &Transaction<'_>,
    env_key: &str,
    directory: &PackageDirectory,
    drops: &mut wamn_platform_identity::auth_changes::AuthDrops,
) -> anyhow::Result<()> {
    bind_apply_package_principal(tx).await?;
    let plan = plan_package_migrations(directory, None)?;
    let installed = load_applied_package(
        tx,
        env_key,
        plan.coordinate.package_id(),
        plan.coordinate.package_version(),
    )
    .await?
    .context("local configuration requires an applied package schema")?;
    let expected = plan
        .pending
        .iter()
        .map(|migration| RecordedMigration {
            ordinal: migration.ordinal,
            relative_path: migration.relative_path.clone(),
            sha256: migration.sha256.clone(),
        })
        .collect::<Vec<_>>();
    ensure!(
        installed.predecessor_version == plan.predecessor_version
            && installed.migrations == expected,
        "local schema inputs changed; recreate the owned disposable target"
    );
    // The relation classification check of apply refuses a manifest that drops
    // a model whose relation the installed migrations create.
    validate_migration_policy(Path::new(""), directory, &plan)?;
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes)?;
    create_history_tables(tx, &manifest).await?;
    reconcile_entity_maps(tx, &plan, &manifest).await?;
    reconcile_record_history_triggers(tx, &manifest).await?;
    ensure_admin_role(tx, env_key, drops).await?;
    Ok(())
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
    let role = wamn_pg_core::quote_ident(&wamn_control_provision::package_owner_role(package));
    for schema in schemas {
        let schema = wamn_schema_control::BareSchemaName::new(schema)
            .context("validate manifest model schema for creation")?;
        tx.batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {} AUTHORIZATION {role}; GRANT USAGE, CREATE ON SCHEMA {} TO {role}", schema.quoted(), schema.quoted()))
            .await
            .with_context(|| format!("ensure package model schema {schema}"))?;
        let unclaimed: bool = tx.query_one(
            "SELECT NOT EXISTS (SELECT 1 FROM catalog.package_definition_owners WHERE schema_name = $1 AND definition_type = 'relation')",
            &[&schema.as_str()],
        ).await?.get(0);
        if unclaimed {
            tx.batch_execute(&format!("ALTER SCHEMA {} OWNER TO {role}", schema.quoted()))
                .await?;
        }
    }
    Ok(())
}

async fn execute(
    tx: &Transaction<'_>,
    statement: &SqlStatement,
    coordinate: &str,
) -> anyhow::Result<()> {
    let result = if statement.params.is_empty() {
        tx.batch_execute(&statement.sql).await
    } else {
        let params = crate::sql_params::as_postgres(&statement.params);
        tx.execute(&statement.sql, &params).await.map(|_| ())
    };
    match result {
        Ok(()) => Ok(()),
        Err(source) if is_package_version_sealed(&source) => Err(ApplyPackageError {
            type_: ApplyPackageErrorType::PackageVersionSealed,
            coordinate: coordinate.to_owned(),
            path: None,
            schema: None,
            relation: None,
            definition_type: None,
            definition: None,
            owner_package: None,
            detail: "already belongs to an effective release; create and apply a new package version for additional migrations".into(),
            source: Some(Box::new(source)),
        }
        .into()),
        Err(source) => Err(source).with_context(|| format!("apply {}", statement.summary)),
    }
}

fn is_package_version_sealed(error: &tokio_postgres::Error) -> bool {
    error.as_db_error().is_some_and(|database| {
        database.code() == &SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE
            && database.message() == PACKAGE_VERSION_SEALED_REFUSAL
    })
}

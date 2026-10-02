//! Apply one package-owned migration stream exactly once per immutable file.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use tokio_postgres::{NoTls, Transaction, error::SqlState};
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql};
use wamn_schema_control::{
    AppliedPackage, PackageDirectory, RecordedMigration, SqlStatement, plan_package_migrations,
    plan_package_registration,
};
use wamn_schema_generator::PackageManifest;

use definition_ownership::reconcile_definition_ownership;
use entity_maps::reconcile_entity_maps;
use migration_policy::{
    MigrationPolicyPlan, validate_definition_ownership_before_apply, validate_migration_policy,
};
use operation_grants::ensure_admin_role;
use package_version::{
    current_package_version, predecessor_not_current_error, predecessor_prefix_error,
};
use record_history::{create_history_tables, reconcile_record_history_triggers};
use registrations::{derive_catalog_registrations, reconcile_package_registrations};
use roles::{assert_host_role, reset_host_role, set_package_owner_role};

mod definition_ownership;
mod entity_maps;
mod error;
mod local_target;
mod migration_policy;
mod operation_grants;
mod package_version;
mod record_history;
mod registrations;
mod roles;
#[cfg(test)]
mod tests;

pub use error::{
    APPLY_PACKAGE_REFUSAL, ApplyPackageError, ApplyPackageErrorType,
    BASE_DEFINITION_MUTATION_REFUSAL, DEFINITION_NOT_FOUND_REFUSAL,
    DEFINITION_OWNER_CONFLICT_REFUSAL, DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL,
    PACKAGE_VERSION_SEALED_REFUSAL, PREDECESSOR_NOT_CURRENT_REFUSAL,
    RELATION_NOT_CLIENT_EXTENSIBLE_REFUSAL,
};
pub(crate) use local_target::record_manifest;
pub use local_target::{applied_migration_drift, local_target_recreate_reason};
pub use package_version::{
    LOCK_PACKAGE_SQL, SELECT_CURRENT_PACKAGE_VERSION_SQL, load_applied_package,
    read_package_directory, register_package,
};

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";

/// Inputs of one exact package application.
#[derive(Debug)]
pub struct ApplyPackageRequest {
    /// Package root containing strict wamn.json and migrations/.
    pub package: PathBuf,

    /// Owner connection to the target project-environment database.
    pub database_url: String,

    /// Tenant stored with the package and migration records.
    pub tenant: String,
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
}

/// Package bytes and migration policy prepared before database mutation.
#[derive(Debug)]
pub(crate) struct PreparedPackage {
    pub root: PathBuf,
    pub directory: PackageDirectory,
    pub manifest: PackageManifest,
    migration_policy: MigrationPolicyPlan,
}

/// Validate one package directory before opening an application transaction.
pub(crate) fn prepare_package(root: &Path) -> anyhow::Result<PreparedPackage> {
    let directory = read_package_directory(root)?;
    let presented = plan_package_migrations(&directory, None)
        .context("validate package directory before database work")?;
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes)
        .context("parse strict package manifest for definition ownership")?;
    wamn_schema_generator::validate_operation_vocabulary(&manifest)
        .context("validate package manifest for registration projection")?;
    let migration_policy = validate_migration_policy(root, &directory, &presented)?;
    Ok(PreparedPackage {
        root: root.to_owned(),
        directory,
        manifest,
        migration_policy,
    })
}

#[derive(Clone, Copy)]
enum ApplicationMode<'a> {
    Production(Option<&'a crate::package_upgrade::AcceptedUpgrade>),
    Qualification,
    Local(&'a str),
}

/// Apply the immutable pending suffix from one package directory.
pub async fn apply_package(request: ApplyPackageRequest) -> anyhow::Result<ApplyOutcome> {
    apply_request(request, ApplicationMode::Production(None)).await
}

/// Apply an installed successor using its exact pre-apply qualification evidence.
pub async fn apply_qualified_package(
    request: ApplyPackageRequest,
    evidence_path: &Path,
) -> anyhow::Result<ApplyOutcome> {
    let evidence = crate::package_upgrade::read_evidence(evidence_path)?;
    apply_request(request, ApplicationMode::Production(Some(&evidence))).await
}

/// Apply a qualified base and its overlay successors atomically, base first.
pub async fn apply_qualified_package_set(
    request: ApplyPackageRequest,
    presented_packages: &[PathBuf],
    evidence_path: &Path,
) -> anyhow::Result<Vec<ApplyOutcome>> {
    let evidence = crate::package_upgrade::read_evidence(evidence_path)?;
    crate::package_upgrade::apply_coordinated(request, presented_packages, evidence).await
}

#[cfg(test)]
pub(crate) async fn apply_qualified_package_set_observed(
    request: ApplyPackageRequest,
    presented_packages: &[PathBuf],
    evidence_path: &Path,
    observed: crate::qualify_upgrade::workload::ServingWorkloads,
) -> anyhow::Result<Vec<ApplyOutcome>> {
    let evidence = crate::package_upgrade::read_evidence_with_observation(evidence_path, observed)?;
    crate::package_upgrade::apply_coordinated(request, presented_packages, evidence).await
}

#[cfg(test)]
pub(crate) async fn apply_qualified_package_observed(
    request: ApplyPackageRequest,
    evidence_path: &Path,
    observed: crate::qualify_upgrade::workload::ServingWorkloads,
) -> anyhow::Result<ApplyOutcome> {
    let evidence = crate::package_upgrade::read_evidence_with_observation(evidence_path, observed)?;
    apply_request(request, ApplicationMode::Production(Some(&evidence))).await
}

/// Apply only to the owned disposable database of upgrade qualification.
pub(crate) async fn apply_qualification_package(
    request: ApplyPackageRequest,
) -> anyhow::Result<ApplyOutcome> {
    apply_request(request, ApplicationMode::Qualification).await
}

/// Apply base and overlay successors atomically to an owned qualification database.
pub(crate) async fn apply_qualification_packages(
    database_url: &str,
    tenant: &str,
    roots: &[PathBuf],
) -> anyhow::Result<Vec<ApplyOutcome>> {
    ensure!(!tenant.is_empty(), "tenant must not be empty");
    ensure!(
        !roots.is_empty(),
        "qualification requires at least one package"
    );
    let packages = roots
        .iter()
        .map(|root| prepare_package(root))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let (mut client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connect to qualification environment")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        let tx = client
            .transaction()
            .await
            .context("begin coordinated qualification apply")?;
        tx.query_one(CLAIM_TENANT_SQL, &[&tenant])
            .await
            .context("claim package tenant")?;
        bind_apply_package_principal(&tx).await?;
        tx.query_one(crate::reconcile_package_data_access::LOCK_SQL, &[])
            .await
            .context("lock data access before coordinated package application")?;
        let families = packages
            .iter()
            .map(|package| package.manifest.package.id.clone())
            .collect::<BTreeSet<_>>();
        for package_id in families {
            tx.query_one(LOCK_PACKAGE_SQL, &[&tenant, &package_id])
                .await
                .context("lock package family")?;
        }
        let mut outcomes = Vec::with_capacity(packages.len());
        // Input order is the qualification coordinator's base-first order.
        for package in &packages {
            outcomes.push(apply_in_transaction(&tx, tenant, package).await?);
        }
        tx.commit()
            .await
            .context("commit coordinated package suffixes")?;
        Ok(outcomes)
    }
    .await;
    drop(client);
    if result.is_err() {
        connection_task.abort();
    } else {
        connection_task
            .await
            .context("join qualification database connection")?
            .context("drive qualification database connection")?;
    }
    result
}

/// Apply one package directory to a local target that wamn dev created.
///
/// The target also takes a changed wamn.json at an applied coordinate, and a
/// migration appended after release membership, while every applied migration
/// stays byte-identical. The database comment then records the current manifest
/// hash of the package.
pub async fn apply_local_package(
    request: ApplyPackageRequest,
    environment: &str,
) -> anyhow::Result<ApplyOutcome> {
    wamn_runtime::local_application::require_local_target(
        &request.database_url,
        &request.tenant,
        environment,
    )
    .await?;
    apply_request(request, ApplicationMode::Local(environment)).await
}

async fn apply_request(
    request: ApplyPackageRequest,
    mode: ApplicationMode<'_>,
) -> anyhow::Result<ApplyOutcome> {
    ensure!(!request.tenant.is_empty(), "tenant must not be empty");
    let package = prepare_package(&request.package)?;
    let (mut client, connection) = tokio_postgres::connect(&request.database_url, NoTls)
        .await
        .context("connect to project environment")?;
    let connection_task = tokio::spawn(connection);
    let result = apply(&mut client, &request.tenant, &package, mode).await;
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
    tenant: &str,
    package: &PreparedPackage,
    mode: ApplicationMode<'_>,
) -> anyhow::Result<ApplyOutcome> {
    let presented = plan_package_migrations(&package.directory, None)?;
    let tx = client.transaction().await.context("begin package apply")?;
    tx.query_one(CLAIM_TENANT_SQL, &[&tenant])
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
        &[&tenant, &presented.coordinate.package_id()],
    )
    .await
    .context("lock package family")?;
    let outcome = apply_prepared(&tx, tenant, package, mode).await?;
    tx.commit().await.context("commit whole package suffix")?;
    Ok(outcome)
}

/// Apply within a transaction whose caller already checked qualification evidence.
///
/// The caller owns the tenant binding, locks, evidence persistence, and commit.
pub(crate) async fn apply_in_transaction(
    tx: &Transaction<'_>,
    tenant: &str,
    package: &PreparedPackage,
) -> anyhow::Result<ApplyOutcome> {
    apply_prepared(tx, tenant, package, ApplicationMode::Qualification).await
}

async fn apply_prepared(
    tx: &Transaction<'_>,
    tenant: &str,
    package: &PreparedPackage,
    mode: ApplicationMode<'_>,
) -> anyhow::Result<ApplyOutcome> {
    let package_root = &package.root;
    let directory = &package.directory;
    let manifest = &package.manifest;
    let migration_policy = &package.migration_policy;
    let registrations = derive_catalog_registrations(manifest);
    let presented =
        plan_package_migrations(directory, None).context("validate package directory")?;
    let package_id = presented.coordinate.package_id().to_owned();
    let package_version = presented.coordinate.package_version().to_owned();
    let coordinate_label = format!("{package_id}@{package_version}");
    let coordinate_text = coordinate_label.as_str();
    let mut local_comment = match mode {
        ApplicationMode::Local(environment) => {
            Some(local_target::lift_release_seal(tx, tenant, environment).await?)
        }
        _ => None,
    };

    let applied = load_applied_package(tx, tenant, &package_id, &package_version).await?;
    let plan = if let Some(applied) = applied.as_ref() {
        let compared = if local_comment.is_some() {
            // A local target takes a changed wamn.json at the same coordinate.
            // The planner still refuses an edited, removed, or reordered
            // applied migration.
            plan_package_migrations(
                directory,
                Some(&AppliedPackage {
                    manifest_sha256: presented.manifest_sha256.clone(),
                    ..applied.clone()
                }),
            )
        } else {
            plan_package_migrations(directory, Some(applied))
        };
        compared.context("compare package bytes with immutable records")?
    } else {
        match current_package_version(tx, tenant, &package_id).await? {
            None => presented,
            Some(current_version) => {
                plan_package_registration(
                    &presented.coordinate,
                    &presented.manifest_sha256,
                    presented.predecessor_version.as_deref(),
                    None,
                    Some(&current_version),
                )
                .map_err(|_| {
                    predecessor_not_current_error(
                        coordinate_text,
                        presented.predecessor_version.as_deref(),
                        &current_version,
                    )
                })?;
                let predecessor = load_applied_package(tx, tenant, &package_id, &current_version)
                    .await?
                    .expect("the selected package-family leaf is an applied package");
                plan_package_migrations(directory, Some(&predecessor)).map_err(|source| {
                    predecessor_prefix_error(
                        coordinate_text,
                        &current_version,
                        Some(source),
                        presented
                            .pending
                            .first()
                            .map(|migration| migration.relative_path.as_str()),
                    )
                })?
            }
        }
    };
    let applied_count = plan.pending.len();
    let migration_changed = !plan.is_noop();
    let pending_paths = plan
        .pending
        .iter()
        .map(|migration| migration.relative_path.as_str())
        .collect::<BTreeSet<_>>();
    let MigrationPolicyPlan {
        mutations,
        deferred,
    } = migration_policy;
    let pending_mutations = mutations
        .iter()
        .filter(|planned| pending_paths.contains(planned.relative_path.as_ref()))
        .collect::<Vec<_>>();

    validate_definition_ownership_before_apply(
        tx,
        tenant,
        coordinate_text,
        &package_id,
        manifest,
        &pending_mutations,
    )
    .await?;
    if let Some(deferred) = deferred {
        // Recreate the owned error only on refusal, preserving its typed source.
        let reparsed = validate_migration_policy(package_root, directory, &plan)?;
        let source = reparsed
            .deferred
            .expect("the same immutable migration bytes retain their policy refusal")
            .source;
        return Err(source)
            .with_context(|| format!("validate {} before apply", deferred.relative_path));
    }

    let accepted_upgrade = match mode {
        ApplicationMode::Production(evidence) => {
            crate::package_upgrade::require_application(
                tx,
                tenant,
                package_root,
                directory,
                &plan,
                evidence,
                false,
            )
            .await?
        }
        ApplicationMode::Qualification | ApplicationMode::Local(_) => None,
    };

    // wamn-yk9l. The platform extensions go in FIRST, while this connection is
    // still the administrator: `CREATE EXTENSION` is refused to a package by
    // the migration policy and is not a privilege the package-owner role holds.
    // Idempotent, so it converges a database provisioned before the list
    // existed.
    tx.batch_execute(&wamn_control_provision::sql::install_platform_extensions_sql())
        .await
        .context("install the platform extensions")?;

    // Package text cannot issue SET ROLE: the pre-apply policy rejects a
    // package escalating itself. This host-issued SET LOCAL ROLE moves in the
    // opposite direction, narrowing the administrator to the existing
    // package-owner role while package DDL runs.
    set_package_owner_role(tx).await?;
    ensure_model_schemas(tx, &plan).await?;
    reset_host_role(tx).await?;
    // catalog.packages is immutable, so a local target keeps the first recorded
    // manifest hash there and records the current one in its comment.
    let registered_manifest_sha256 = match (&local_comment, &applied) {
        (Some(_), Some(applied)) => &applied.manifest_sha256,
        _ => &plan.manifest_sha256,
    };
    let package_inserted = register_package(
        tx,
        tenant,
        &plan.coordinate,
        registered_manifest_sha256,
        plan.predecessor_version.as_deref(),
    )
    .await
    .context("register package before applying migrations")?;
    for statement in &plan.statements {
        // The planner carries exact package bytes as its parameter-free batch
        // statements; every host-authored record statement has binds.
        if statement.params.is_empty() {
            set_package_owner_role(tx).await?;
            execute(tx, statement, coordinate_text).await?;
            reset_host_role(tx).await?;
        } else {
            assert_host_role(tx).await?;
            execute(tx, statement, coordinate_text).await?;
        }
    }
    assert_host_role(tx).await?;
    let ownership_changed = reconcile_definition_ownership(
        tx,
        tenant,
        coordinate_text,
        &package_id,
        manifest,
        &pending_mutations,
    )
    .await?;
    let history_changed = create_history_tables(tx, manifest).await?;
    reconcile_entity_maps(tx, &plan, manifest).await?;
    let triggers_changed = reconcile_record_history_triggers(tx, manifest).await?;
    let admin_role_changed = ensure_admin_role(tx, tenant).await?;
    let registrations_changed =
        reconcile_package_registrations(tx, tenant, &package_id, &registrations).await?;
    let comment_changed = match local_comment.as_mut() {
        Some(comment) => {
            local_target::record_manifest(tx, comment, coordinate_text, &plan.manifest_sha256)
                .await?
        }
        None => false,
    };
    if let Some(evidence) = &accepted_upgrade {
        crate::package_upgrade::persist(tx, evidence).await?;
    }
    Ok(ApplyOutcome {
        package_id,
        package_version,
        migrations_applied: applied_count,
        changed: package_inserted
            || migration_changed
            || ownership_changed
            || history_changed
            || triggers_changed
            || admin_role_changed
            || registrations_changed
            || comment_changed,
    })
}

/// Reconcile mutable local configuration only after confirming the installed schema.
pub async fn reconcile_local_package_configuration(
    tx: &Transaction<'_>,
    tenant: &str,
    directory: &PackageDirectory,
) -> anyhow::Result<()> {
    bind_apply_package_principal(tx).await?;
    let plan = plan_package_migrations(directory, None)?;
    let installed = load_applied_package(
        tx,
        tenant,
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
    ensure_admin_role(tx, tenant).await?;
    reconcile_package_registrations(
        tx,
        tenant,
        plan.coordinate.package_id(),
        &derive_catalog_registrations(&manifest),
    )
    .await?;
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
        let schema = wamn_schema_control::BareSchemaName::new(schema)
            .context("validate manifest model schema for creation")?;
        tx.batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {}", schema.quoted()))
            .await
            .with_context(|| format!("ensure package model schema {schema}"))?;
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
            predecessor_version: None,
            current_version: None,
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

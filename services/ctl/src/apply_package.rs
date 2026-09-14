//! Apply one package-owned migration stream exactly once per immutable file.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use clap::Args;
use tokio_postgres::{NoTls, Transaction, error::SqlState};
use wamn_control_provision::{PlatformComponent, bind_platform_principal_sql};
use wamn_schema_control::{
    PackageDirectory, RecordedMigration, SqlStatement, plan_package_migrations,
    plan_package_registration,
};
use wamn_schema_generator::PackageManifest;

use definition_ownership::reconcile_definition_ownership;
use entity_maps::reconcile_entity_maps;
use migration_policy::{
    MigrationPolicyPlan, validate_definition_ownership_before_apply, validate_migration_policy,
};
use operation_grants::reconcile_package_operation_grants;
use package_version::{
    current_package_version, predecessor_not_current_error, predecessor_prefix_error,
};
use record_history::{create_history_tables, reconcile_record_history_triggers};
use registrations::{derive_catalog_registrations, reconcile_package_registrations};
use roles::{assert_host_role, reset_host_role, set_package_owner_role};

mod definition_ownership;
mod entity_maps;
mod error;
mod migration_policy;
mod operation_grants;
mod package_version;
mod record_history;
mod registrations;
mod roles;
#[cfg(test)]
mod tests;

pub use error::{
    APPLY_PACKAGE_REFUSAL, ApplyPackageError, ApplyPackageErrorKind,
    BASE_DEFINITION_MUTATION_REFUSAL, DEFINITION_NOT_FOUND_REFUSAL,
    DEFINITION_OWNER_CONFLICT_REFUSAL, DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL,
    PACKAGE_VERSION_SEALED_REFUSAL, PREDECESSOR_NOT_CURRENT_REFUSAL,
    RELATION_NOT_CLIENT_EXTENSIBLE_REFUSAL,
};
pub(crate) use package_version::{
    LOCK_PACKAGE_SQL, SELECT_CURRENT_PACKAGE_VERSION_SQL, load_applied_package,
    read_package_directory, register_package,
};

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";

/// Apply the immutable pending suffix from one package directory.
#[derive(Debug, Args)]
pub struct ApplyPackageArgs {
    /// Package root containing strict wamn.json and migrations/.
    #[arg(long)]
    pub package: PathBuf,

    /// Owner connection to the target project-environment database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub database_url: String,

    /// Tenant stored with the package and migration records.
    #[arg(long)]
    pub tenant: String,
}

#[derive(Debug)]
struct ApplyOutcome {
    migrations_applied: usize,
    changed: bool,
}

pub async fn run(args: ApplyPackageArgs) -> anyhow::Result<()> {
    ensure!(!args.tenant.is_empty(), "tenant must not be empty");
    let directory = read_package_directory(&args.package)?;
    let presented = plan_package_migrations(&directory, None)
        .context("validate package directory before database work")?;
    let manifest = PackageManifest::from_slice(&directory.manifest_bytes)
        .context("parse strict package manifest for definition ownership")?;
    let migration_policy = validate_migration_policy(&args.package, &directory, &presented)?;
    let coordinate = presented.coordinate.clone();
    let coordinate_text = format!(
        "{}@{}",
        coordinate.package_id(),
        coordinate.package_version()
    );

    let (mut client, connection) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("connect to project environment")?;
    let connection_task = tokio::spawn(connection);
    let result = apply(
        &mut client,
        &args.tenant,
        &coordinate_text,
        &directory,
        &manifest,
        migration_policy,
    )
    .await;
    drop(client);
    if result.is_err() {
        connection_task.abort();
    } else {
        connection_task
            .await
            .context("join package database connection")?
            .context("drive package database connection")?;
    }
    let outcome = result?;
    println!(
        "applied {coordinate_text}: {} migration(s){}",
        outcome.migrations_applied,
        if !outcome.changed {
            " (already converged)"
        } else {
            ""
        }
    );
    Ok(())
}

async fn apply(
    client: &mut tokio_postgres::Client,
    tenant: &str,
    coordinate_text: &str,
    directory: &PackageDirectory,
    manifest: &PackageManifest,
    migration_policy: MigrationPolicyPlan,
) -> anyhow::Result<ApplyOutcome> {
    wamn_schema_generator::validate_operation_vocabulary(manifest)
        .context("validate package manifest for registration projection")?;
    let registrations = derive_catalog_registrations(manifest);
    let presented =
        plan_package_migrations(directory, None).context("validate package directory")?;
    let package_id = presented.coordinate.package_id().to_owned();
    let package_version = presented.coordinate.package_version().to_owned();
    let tx = client.transaction().await.context("begin package apply")?;
    tx.query_one(CLAIM_TENANT_SQL, &[&tenant])
        .await
        .context("claim package tenant")?;
    bind_apply_package_principal(&tx).await?;
    tx.query_one(LOCK_PACKAGE_SQL, &[&tenant, &package_id])
        .await
        .context("lock package family")?;

    let applied = load_applied_package(&tx, tenant, &package_id, &package_version).await?;
    let plan = if let Some(applied) = applied.as_ref() {
        plan_package_migrations(directory, Some(applied))
            .context("compare package bytes with immutable records")?
    } else {
        match current_package_version(&tx, tenant, &package_id).await? {
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
                let predecessor = load_applied_package(&tx, tenant, &package_id, &current_version)
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
        &tx,
        tenant,
        coordinate_text,
        &package_id,
        manifest,
        &pending_mutations,
    )
    .await?;
    if let Some(deferred) = deferred {
        return Err(deferred.source)
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

    // Package text cannot issue SET ROLE: the pre-apply policy rejects a
    // package escalating itself. This host-issued SET LOCAL ROLE moves in the
    // opposite direction, narrowing the administrator to the existing
    // package-owner role while package DDL runs.
    set_package_owner_role(&tx).await?;
    ensure_model_schemas(&tx, &plan).await?;
    reset_host_role(&tx).await?;
    let package_inserted = register_package(
        &tx,
        tenant,
        &plan.coordinate,
        &plan.manifest_sha256,
        plan.predecessor_version.as_deref(),
    )
    .await
    .context("register package before applying migrations")?;
    for statement in &plan.statements {
        // The planner carries exact package bytes as its parameter-free batch
        // statements; every host-authored record statement has binds.
        if statement.params.is_empty() {
            set_package_owner_role(&tx).await?;
            execute(&tx, statement, &coordinate_text).await?;
            reset_host_role(&tx).await?;
        } else {
            assert_host_role(&tx).await?;
            execute(&tx, statement, &coordinate_text).await?;
        }
    }
    assert_host_role(&tx).await?;
    let ownership_changed = reconcile_definition_ownership(
        &tx,
        tenant,
        coordinate_text,
        &package_id,
        manifest,
        &pending_mutations,
    )
    .await?;
    let history_changed = create_history_tables(&tx, manifest).await?;
    reconcile_entity_maps(&tx, &plan, manifest).await?;
    let triggers_changed = reconcile_record_history_triggers(&tx, manifest).await?;
    let operation_grants =
        reconcile_package_operation_grants(&tx, &directory.manifest_bytes, tenant).await?;
    let registrations_changed =
        reconcile_package_registrations(&tx, tenant, &package_id, &registrations).await?;
    tx.commit().await.context("commit whole package suffix")?;
    Ok(ApplyOutcome {
        migrations_applied: applied_count,
        changed: package_inserted
            || migration_changed
            || ownership_changed
            || history_changed
            || triggers_changed
            || !operation_grants.is_noop()
            || registrations_changed,
    })
}

/// Reconcile mutable local configuration only after confirming the installed schema.
pub(crate) async fn reconcile_local_package_configuration(
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
    reconcile_package_operation_grants(tx, &directory.manifest_bytes, tenant).await?;
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
/// transaction, so its writes, including operation grants, record that
/// component.
async fn bind_apply_package_principal(tx: &Transaction<'_>) -> anyhow::Result<()> {
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
            kind: ApplyPackageErrorKind::PackageVersionSealed,
            coordinate: coordinate.to_owned(),
            predecessor_version: None,
            current_version: None,
            path: None,
            schema: None,
            relation: None,
            definition_kind: None,
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

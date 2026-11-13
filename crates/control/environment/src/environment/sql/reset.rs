//! Reconstruct a disposable environment under its existing CNPG database name.

use sqlx::Row as _;
use std::collections::BTreeMap;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wamn_compiler::manifest::PackageManifest;
use wamn_package_control::apply_package;
use wamn_postgres::connect;

use super::super::{Platform, analyse, document::EnvironmentDocument, ensure, stage};
use super::schema::{self, Candidate, SchemaSource};
use crate::provision_project_env::registry;

/// Immutable reconstruction inputs, not deployment progress.
#[derive(Debug, Serialize, Deserialize)]
struct Inputs {
    substrate: crate::platform_migrations::ProjectReset,
    sources: BTreeMap<String, SchemaSource>,
}

/// The registry retains this identity across SQL failures and retries.
#[derive(Debug, Serialize, Deserialize)]
struct Pending {
    initialization_identity: String,
    inputs: Inputs,
}

/// Resolve every retained schema and running release before recording reset inputs.
/// The caller holds the environment lifecycle lock.
pub async fn prepare(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: &str,
    release: &str,
    admin_url: &str,
) -> anyhow::Result<()> {
    let (release, selected) =
        schema::release_candidates(platform, document, application, Some(release)).await?;
    let state = schema::state(platform, document).await?;
    ensure!(
        state.disposable == Some(true),
        "environment is not recorded as disposable"
    );
    let diff = schema::plan(document, application, Some(release), &selected, &state)?;
    let candidates = schema::reset_candidates(platform, &diff, selected).await?;
    prepare_inputs(
        platform,
        document,
        candidates,
        Inputs {
            substrate: recorded_substrate(
                platform,
                document,
                &crate::platform_migrations::embedded_sources(
                    wamn_control_provision::schema_migrations::MigrationDatabase::Project,
                ),
            )
            .await?,
            sources: diff.reset_sources,
        },
        diff.database_instance.as_deref(),
        admin_url,
    )
    .await
}

/// Reconstruct retained installed SQL even when every application is stopped.
pub async fn prepare_retained(
    platform: &Platform,
    document: &EnvironmentDocument,
    admin_url: &str,
) -> anyhow::Result<()> {
    prepare_retained_with_migrations(
        platform,
        document,
        admin_url,
        &crate::platform_migrations::embedded_sources(
            wamn_control_provision::schema_migrations::MigrationDatabase::Project,
        ),
    )
    .await
}

/// Prepare retained SQL with the executing image's exact project files.
pub async fn prepare_retained_with_migrations(
    platform: &Platform,
    document: &EnvironmentDocument,
    admin_url: &str,
    migrations: &[wamn_schema_control::MigrationSource],
) -> anyhow::Result<()> {
    let state = schema::state(platform, document).await?;
    ensure!(
        state.disposable == Some(true),
        "environment is not recorded as disposable"
    );
    ensure!(
        state.unknowns.is_empty(),
        "installed schema is unknown: {}",
        state.unknowns.join("; ")
    );
    let sources = state
        .packages
        .into_iter()
        .map(|(id, package)| {
            (
                id,
                SchemaSource {
                    version: package.coordinate.package_version().to_owned(),
                    artifact: package.artifact_digest,
                    fingerprint: package.physical_schema_fingerprint,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut candidates = Vec::new();
    for (id, source) in &sources {
        candidates
            .push(schema::open(platform, id, &source.version, source.artifact.as_deref()).await?);
    }
    prepare_inputs(
        platform,
        document,
        candidates,
        Inputs {
            substrate: recorded_substrate(platform, document, migrations).await?,
            sources,
        },
        state.database_instance.as_deref(),
        admin_url,
    )
    .await
}

async fn prepare_inputs(
    platform: &Platform,
    document: &EnvironmentDocument,
    candidates: Vec<Candidate>,
    inputs: Inputs,
    expected: Option<&str>,
    admin_url: &str,
) -> anyhow::Result<()> {
    validate_candidates(document, &candidates, &inputs.sources)?;
    resolve_releases(platform, document).await?;
    let inputs = serde_json::to_value(inputs)?;
    let mut system = connect(&platform.system_database_url).await?;
    let active = crate::provision_project_env::read_project_env_instance(
        &platform.system_database_url,
        &document.triple(),
    )
    .await?;
    ensure!(
        Some(database(document, &active).as_str()) == expected,
        "active database changed during input resolution"
    );
    registry::record_reset(&mut system, &document.triple(), &active, &inputs).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        crate::provision_project_env::role_posture_sql(),
    ))
    .execute(&mut connect(admin_url).await?)
    .await?;
    Ok(())
}

async fn recorded_substrate(
    platform: &Platform,
    document: &EnvironmentDocument,
    sources: &[wamn_schema_control::MigrationSource],
) -> anyhow::Result<crate::platform_migrations::ProjectReset> {
    let url = ensure::project_url(platform, &document.triple()).await?;
    let mut project = analyse::connect_project(&url).await?;
    crate::platform_migrations::ProjectReset::read(&mut project, sources).await
}

/// Select only pinned applied files during reset initialization and retries.
pub(crate) async fn migration_sources(
    system: &mut sqlx::PgConnection,
    triple: &wamn_registry::Triple,
    sources: &[wamn_schema_control::MigrationSource],
) -> anyhow::Result<Option<Vec<wamn_schema_control::MigrationSource>>> {
    let pending = registry::pending_database(system, triple).await?;
    pending
        .map(|pending| {
            let pending: Pending = serde_json::from_value(pending)?;
            pending.inputs.substrate.sources(sources.to_vec())
        })
        .transpose()
}

fn database(document: &EnvironmentDocument, instance: &str) -> String {
    wamn_control_provision::project_env_database_name(
        &document.org,
        &document.project,
        &document.env,
        instance,
    )
}

pub(in crate::environment) async fn recorded_cluster(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<String> {
    let policy = crate::provision_project_env::read_project_env_policy(
        &platform.system_database_url,
        &document.triple(),
    )
    .await?;
    crate::provision_project_env::resolve_cluster(
        &platform.system_database_url,
        &document.org,
        &policy,
    )
    .await
}

async fn read_pending(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<Pending> {
    let mut system = connect(&platform.system_database_url).await?;
    let row = analyse::read_row(&mut system, document)
        .await?
        .context("environment is not recorded")?;
    ensure!(row.disposable, "environment is not disposable");
    let pending: Pending = serde_json::from_value(
        registry::pending_database(&mut system, &document.triple())
            .await?
            .context("no pending database")?,
    )?;
    Ok(pending)
}

fn validate_candidates(
    document: &EnvironmentDocument,
    candidates: &[Candidate],
    sources: &BTreeMap<String, SchemaSource>,
) -> anyhow::Result<(Vec<PackageManifest>, Vec<usize>)> {
    ensure!(
        sources.values().all(|source| source.artifact.is_some()),
        "deployed reset requires published schema sources"
    );
    schema::validate_candidates(document, candidates, sources)
}

async fn resolve_releases(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<
    Vec<(
        String,
        String,
        wamn_catalog::ServingManifest,
        Vec<PackageManifest>,
    )>,
> {
    let mut system = connect(&platform.system_database_url).await?;
    let mut releases = Vec::new();
    for (application, selection) in &document.applications {
        if let super::super::document::DeclaredRelease::Digest(digest) = &selection.current {
            let manifest = analyse::pull_release_manifest(platform, digest).await?;
            let selected = analyse::select_application(document, Some(application))?;
            let refusals = analyse::application_refusals(
                &selected.membership(),
                manifest
                    .release
                    .packages
                    .iter()
                    .map(wamn_catalog::PackageCoordinate::package_id),
            );
            ensure!(refusals.is_empty(), "{}", refusals.join("; "));
            let (packages, _) = analyse::read_release_packages(platform, &manifest).await?;
            super::super::bindings::analyse(&mut system, &selected, &packages).await?;
            releases.push((application.clone(), digest.clone(), manifest, packages));
        }
    }
    Ok(releases)
}

/// Validate a retry image before the caller stops writers or recreates a database.
pub async fn validate_pending_substrate(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<()> {
    let pending = read_pending(platform, document).await?;
    pending
        .inputs
        .substrate
        .validate(&crate::platform_migrations::embedded_sources(
            wamn_control_provision::schema_migrations::MigrationDatabase::Project,
        ))?;
    Ok(())
}

/// Restore the same database from pinned inputs after CNPG reports success.
/// Package SQL and execution records commit together through apply-package.
pub async fn initialize(
    platform: &Platform,
    document: &EnvironmentDocument,
    admin_url: &str,
) -> anyhow::Result<()> {
    initialize_with_migrations(
        platform,
        document,
        admin_url,
        &crate::platform_migrations::embedded_sources(
            wamn_control_provision::schema_migrations::MigrationDatabase::Project,
        ),
    )
    .await
}

/// Restore pinned SQL with explicit files from the executing image.
pub async fn initialize_with_migrations(
    platform: &Platform,
    document: &EnvironmentDocument,
    admin_url: &str,
    migrations: &[wamn_schema_control::MigrationSource],
) -> anyhow::Result<()> {
    let pending = read_pending(platform, document).await?;
    pending.inputs.substrate.validate(migrations)?;
    let instance = crate::provision_project_env::read_project_env_instance(
        &platform.system_database_url,
        &document.triple(),
    )
    .await?;
    let mut candidates = Vec::new();
    for (id, source) in &pending.inputs.sources {
        candidates
            .push(schema::open(platform, id, &source.version, source.artifact.as_deref()).await?);
    }
    let (packages, order) = validate_candidates(document, &candidates, &pending.inputs.sources)?;
    let releases = resolve_releases(platform, document).await?;
    let database = database(document, &instance);
    let project_url =
        wamn_postgres::database_configuration(&platform.system_database_url, &database)?;
    let target_admin = wamn_postgres::database_configuration(admin_url, &database)?;
    let mut target = connect(&target_admin).await?;
    if sqlx::query("SELECT to_regclass('platform.schema_baseline') IS NOT NULL")
        .fetch_one(&mut target)
        .await?
        .get::<bool, _>(0)
    {
        let identity: String =
            sqlx::query("SELECT installed_at::text FROM platform.schema_baseline WHERE singleton")
                .fetch_one(&mut target)
                .await?
                .get(0);
        ensure!(
            identity == pending.initialization_identity,
            "database has not been recreated for this reset"
        );
    }
    let mut admin = connect(admin_url).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        crate::provision_project_env::privilege_sql(&database),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    ensure::ensure_project_database_with_migrations(
        platform, document, &packages, &mut admin, admin_url, migrations,
    )
    .await?;
    let changes = platform.auth_changes().await?;
    let drops = super::super::bindings::project_authority(
        &platform.system_database_url,
        &project_url,
        document,
    )
    .await?;
    changes.publish(drops).await;
    for index in order {
        let candidate = &candidates[index];
        apply_package::apply_package(apply_package::ApplyPackageRequest {
            component_source: platform.component_source()?,
            package: candidate.root.clone(),
            changes: changes.clone(),
            database_url: project_url.clone(),
            org: document.org.clone(),
            project: document.project.clone(),
            environment: document.env.clone(),
        })
        .await?;
    }
    // An environment with no applications has no package data authority to reconcile.
    if !candidates.is_empty() {
        crate::reconcile_package_data_access::reconcile_package_data_access(
            crate::reconcile_package_data_access::ReconcilePackageDataAccessRequest {
                packages: candidates
                    .iter()
                    .map(|candidate| candidate.root.clone())
                    .collect(),
                database_url: project_url.clone(),
                org: document.org.clone(),
                project: document.project.clone(),
                environment: document.env.clone(),
            },
        )
        .await?;
    }
    let mut defaults = Vec::new();
    for application in document.applications.keys() {
        let selected = analyse::select_application(document, Some(application))?;
        defaults.extend(stage::sequence_defaults(
            &selected.membership(),
            packages.iter().filter(|package| {
                selected.applications[application]
                    .packages
                    .contains(&package.package.id)
            }),
        )?);
    }
    stage::create_number_sequences(&project_url, &defaults).await?;
    let project_env_key =
        wamn_control_provision::project_env_key(&document.org, &document.project, &document.env);
    for (application, digest, manifest, code_packages) in releases {
        stage::materialize_application_principals(
            &platform.system_database_url,
            &project_url,
            &project_env_key,
            &manifest,
            &code_packages,
            &changes,
        )
        .await?;
        super::super::expand::prepare_at(
            platform,
            document,
            &application,
            &digest,
            &manifest,
            &code_packages,
            &project_url,
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .await?;
    }
    Ok(())
}

/// Resolve through the installed SQL image before stopping any workload.
async fn prepare_deployed(
    platform: &Platform,
    document: &EnvironmentDocument,
    candidate: Option<(&str, &str)>,
) -> anyhow::Result<Pending> {
    let mut system = connect(&platform.system_database_url).await?;
    if registry::pending_database(&mut system, &document.triple())
        .await?
        .is_none()
    {
        let private = ensure::PrivateDirectory::create()?;
        let chart = crate::platform::artifact_plan::select(&platform.chart)?;
        let cluster = recorded_cluster(platform, document).await?;
        super::run_deployed(
            platform,
            document,
            super::Operation::Reset { candidate },
            chart.image("ctl")?,
            &cluster,
            &private.path("reset-inputs"),
        )
        .await?;
    }
    let pending = read_pending(platform, document).await?;
    Ok(pending)
}

async fn match_candidate(
    platform: &Platform,
    document: &EnvironmentDocument,
    candidate: Option<(&str, &str)>,
    pending: &Pending,
) -> anyhow::Result<()> {
    if let Some((application, release)) = candidate {
        let (_, candidates) =
            schema::release_candidates(platform, document, application, Some(release)).await?;
        for candidate in candidates {
            let manifest = PackageManifest::from_slice(&candidate.directory.manifest_bytes)?;
            let source = pending
                .inputs
                .sources
                .get(&manifest.package.id)
                .context("pending schema source")?;
            ensure!(
                source.fingerprint == candidate.fingerprint
                    && source.artifact == candidate.root.artifact_digest,
                "finish the pending reset before applying different schema inputs"
            );
        }
    }
    Ok(())
}

/// Recreate the existing CNPG database, then restore its recorded inputs and users.
pub async fn reset_deployed(
    platform: &Platform,
    file: &std::path::Path,
    candidate: Option<(&str, &str)>,
) -> anyhow::Result<Value> {
    let admin = std::env::var("WAMN_SYSTEM_ADMIN_URL").context("reset administrator URL")?;
    Box::pin(reset_deployed_with_admin(platform, file, candidate, &admin)).await
}

/// Reset with the existing administrator credential protected by the worker manifest.
pub async fn reset_deployed_with_admin(
    platform: &Platform,
    file: &std::path::Path,
    candidate: Option<(&str, &str)>,
    admin_url: &str,
) -> anyhow::Result<Value> {
    let document = EnvironmentDocument::read_deployed(
        platform,
        file,
        candidate.map(|(application, _)| application),
    )
    .await?;
    let private = ensure::PrivateDirectory::create()?;
    let mut platform = platform.clone();
    super::super::runtime::resolve_target(&mut platform, &document, &private.path(".")).await?;
    eprintln!(
        "Reset stops all applications until reconnection. Data loss is irreversible: application rows, manually created workflow definitions, queued runs, and queued events are deleted. Affected applications: {}",
        document
            .applications
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    );
    let pending = prepare_deployed(&platform, &document, candidate).await?;
    let mut completed = Vec::new();
    let result = async {
        let lock = super::super::lock::LifecycleLock::acquire_wait(
            &platform.system_database_url,
            &document.triple(),
        )
        .await?;
        let current = EnvironmentDocument::read_target(&platform, &document, None).await?;
        ensure!(
            current == document,
            "environment document changed during reset"
        );
        ensure!(
            serde_json::to_value(read_pending(&platform, &document).await?)?
                == serde_json::to_value(&pending)?,
            "reset inputs changed"
        );
        match_candidate(&platform, &document, candidate, &pending).await?;
        let mut candidates = Vec::new();
        for (id, source) in &pending.inputs.sources {
            candidates.push(
                schema::open(&platform, id, &source.version, source.artifact.as_deref()).await?,
            );
        }
        validate_candidates(&document, &candidates, &pending.inputs.sources)?;
        resolve_releases(&platform, &document).await?;
        crate::enable_cdc_project_env::pause_event_reader(
            &platform.system_database_url,
            &document.triple(),
        )
        .await?;
        stop_users(&platform, &document).await?;
        completed.push("workloads stopped");
        if needs_recreation(&platform, &document).await? {
            recreate(&platform, &document, &mut completed, admin_url).await?;
        } else {
            completed.push("pinned initialization retained");
        }
        lock.release().await;
        let chart = crate::platform::artifact_plan::select(&platform.chart)?;
        let cluster = recorded_cluster(&platform, &document).await?;
        super::run_deployed(
            &platform,
            &document,
            super::Operation::Reset { candidate: None },
            chart.image("ctl")?,
            &cluster,
            &private.path("reset-sql"),
        )
        .await?;
        completed.push("SQL restored");
        let lock = super::super::lock::LifecycleLock::acquire_wait(
            &platform.system_database_url,
            &document.triple(),
        )
        .await?;
        let state = schema::state(&platform, &document).await?;
        ensure!(
            state.initialization_identity.as_deref() == Some(&pending.initialization_identity),
            "database initialization changed before reconnect"
        );
        let result = reconnect(&platform, &document, &mut completed, admin_url).await;
        lock.release().await;
        result
    }
    .await;
    result.with_context(|| {
        format!(
            "reset outcome; completed: {}; resume an incomplete reset only while pending_database exists",
            completed.join(", ")
        )
    })
}

async fn stop_users(platform: &Platform, document: &EnvironmentDocument) -> anyhow::Result<()> {
    crate::enable_cdc_project_env::stop_reader(platform, &document.triple()).await?;
    let shared = ensure::environment_host_name(&document.org, &document.project, &document.env)?;
    ensure::kubectl(
        platform,
        &platform.target.namespace,
        &[
            "delete",
            "workloaddeployment",
            &wamn_registry::name::resource_suffix(&shared, "materializer"),
            "--ignore-not-found",
            "--wait=true",
            "--timeout=120s",
        ],
        None,
    )
    .await?;
    let mut groups = std::collections::BTreeSet::from([shared]);
    for application in document.applications.keys() {
        groups.insert(analyse::application_release_name(document, application)?);
    }
    for name in &groups {
        stop_group(platform, name).await?;
    }
    Ok(())
}

/// A matching durable initialization identity forbids another destructive recreation.
pub async fn needs_recreation(
    platform: &Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<bool> {
    let pending = read_pending(platform, document).await?;
    let project_url = ensure::project_url(platform, &document.triple()).await?;
    let name = wamn_postgres::identity(&project_url)?.1;
    let mut system = connect(&platform.system_database_url).await?;
    // An interrupted absent operation leaves the owned database closed to connections.
    let database = sqlx::query("SELECT datallowconn FROM pg_database WHERE datname=$1")
        .bind(&name)
        .fetch_optional(&mut system)
        .await?;
    if database.is_none_or(|row| !row.get::<bool, _>(0)) {
        return Ok(true);
    }
    let mut project = connect(&project_url).await?;
    if !sqlx::query("SELECT to_regclass('platform.schema_baseline') IS NOT NULL")
        .fetch_one(&mut project)
        .await?
        .get::<bool, _>(0)
    {
        return Ok(true);
    }
    let identity: String =
        sqlx::query("SELECT installed_at::text FROM platform.schema_baseline WHERE singleton")
            .fetch_one(&mut project)
            .await?
            .get(0);
    Ok(identity != pending.initialization_identity)
}

async fn recreate(
    platform: &Platform,
    document: &EnvironmentDocument,
    completed: &mut Vec<&str>,
    admin_url: &str,
) -> anyhow::Result<()> {
    let instance = crate::provision_project_env::read_project_env_instance(
        &platform.system_database_url,
        &document.triple(),
    )
    .await?;
    let name = database(document, &instance);
    let cluster = recorded_cluster(platform, document).await?;
    let resource: Value = wamn_kubernetes::kubectl::get(
        &platform.target.kubeconfig,
        &platform.target.context,
        &platform.database_namespace,
        "database",
        &name,
    )
    .await?
    .context("existing CNPG Database")?;
    ensure!(
        resource["spec"]["name"] == name
            && resource["spec"]["cluster"]["name"] == cluster
            && resource["metadata"]["annotations"]["wamn.environment"]
                == document.triple().to_string(),
        "Database resource has different ownership"
    );
    let postgres = wamn_postgres::database_configuration(admin_url, "postgres")?;
    let mut admin = connect(&postgres).await?;
    let capture = wamn_control_provision::cdc_object_name(
        &document.org,
        &document.project,
        &document.env,
        &instance,
    );
    ensure::kubectl(
        platform,
        &platform.database_namespace,
        &[
            "patch",
            "database",
            &name,
            "--type",
            "merge",
            "-p",
            r#"{"spec":{"allowConnections":false}}"#,
        ],
        None,
    )
    .await?;
    if sqlx::query("SELECT EXISTS(SELECT FROM pg_database WHERE datname=$1)")
        .bind(&name)
        .fetch_one(&mut admin)
        .await?
        .get::<bool, _>(0)
    {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "ALTER DATABASE {} ALLOW_CONNECTIONS false",
            postgres_protocol::escape::escape_identifier(&name)
        )))
        .execute(&mut admin)
        .await
        .map(drop)?;
    }
    sqlx::query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname=$1 AND pid<>pg_backend_pid()").bind(&name).fetch_all(&mut admin).await?;
    sqlx::query(
        "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name=$1",
    )
    .bind(&capture)
    .fetch_all(&mut admin)
    .await?;
    for (desired, exists) in [("absent", false), ("present", true)] {
        let patch =
            serde_json::json!({"spec":{"ensure":desired,"allowConnections":exists}}).to_string();
        ensure::kubectl(
            platform,
            &platform.database_namespace,
            &["patch", "database", &name, "--type", "merge", "-p", &patch],
            None,
        )
        .await?;
        ensure::wait_applied(platform, "database", &name).await?;
        tokio::time::timeout(std::time::Duration::from_secs(120), async {
            loop {
                let found: bool =
                    sqlx::query("SELECT EXISTS(SELECT FROM pg_database WHERE datname=$1)")
                        .bind(&name)
                        .fetch_one(&mut admin)
                        .await?
                        .get(0);
                if found == exists {
                    break Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await
        .context("wait for CNPG database operation")??;
        completed.push(if exists {
            "database present"
        } else {
            "database absent: old data destroyed"
        });
    }
    Ok(())
}

/// Reconnect capture recovery through the registry owner's administrator credential.
pub async fn reconnect_capture_gap(
    platform: &Platform,
    document: &EnvironmentDocument,
    admin_url: &str,
) -> anyhow::Result<()> {
    let owner_url = wamn_postgres::database_configuration(
        admin_url,
        &wamn_postgres::identity(&platform.system_database_url)?.1,
    )?;
    crate::capture_gap::close_open_capture_gap(&crate::capture_gap::CloseCaptureGapRequest {
        org: document.org.clone(),
        project: document.project.clone(),
        env: document.env.clone(),
        system_database_url: owner_url,
    })
    .await
}

async fn reconnect(
    platform: &Platform,
    document: &EnvironmentDocument,
    completed: &mut Vec<&str>,
    admin_url: &str,
) -> anyhow::Result<Value> {
    let pending = read_pending(platform, document).await?;
    let pending_value = serde_json::to_value(&pending)?;
    let instance = crate::provision_project_env::read_project_env_instance(
        &platform.system_database_url,
        &document.triple(),
    )
    .await?;
    let name = database(document, &instance);
    let mut system = connect(&platform.system_database_url).await?;
    let project_admin = wamn_postgres::database_configuration(admin_url, &name)?;
    // The installed SQL image owns the substrate. Reconnect uses the current
    // credential owner even when that image predates credential restoration.
    ensure::restore_project_database_access(platform, document, project_admin.as_str()).await?;
    ensure::prepare_cdc(
        platform,
        document,
        &mut connect(admin_url).await?,
        project_admin.as_str(),
    )
    .await?;
    reconnect_capture_gap(platform, document, admin_url).await?;
    ensure::ensure_cdc_inputs(platform, document, Some(admin_url)).await?;
    completed.push("CDC rebuilt");
    let broker = platform
        .events
        .as_ref()
        .context("reset requires the environment event broker")?;
    let broker = async_nats::jetstream::new(broker.nats.connect().await?);
    wamn_broker::streams::discard(&broker, &document.triple()).await?;
    completed.push("old event backlog discarded");
    registry::finish_database(&mut system, &document.triple(), &pending_value).await?;
    completed.push("reset complete");
    ensure::ensure_cdc_inputs(platform, document, Some(admin_url))
        .await
        .context("reset complete; reapply the unchanged selections to resume event processing")?;
    resume_users(platform, document)
        .await
        .context("reset complete; reapply the named unchanged selections, without another reset")?;
    completed.push("workloads resumed");
    Ok(serde_json::json!({"database":name,"pending_database":null,
            "initialization_identity":pending.initialization_identity,
            "data_loss":"The reset deletes old rows, queued runs, manually authored workflow definitions, and queued events.",
            "result":"reset"}))
}

async fn resume_users(platform: &Platform, document: &EnvironmentDocument) -> anyhow::Result<()> {
    use super::super::{readiness, write};
    // The reset is complete. Name the first failure for ordinary application reapply.
    for (application, selection) in &document.applications {
        if matches!(
            selection.current,
            super::super::document::DeclaredRelease::None
        ) {
            continue;
        }
        let analysis = analyse::analyse(platform, document, Some(application)).await?;
        let budget = std::time::Duration::from_secs(u64::try_from(
            analysis.authorities.policy.readiness_budget_seconds,
        )?);
        tokio::time::timeout(budget, async {
            let release = analysis
                .authorities
                .release
                .as_ref()
                .context("application release")?;
            let prepared = write::prepare(platform, document, &analysis, release).await?;
            let written = write::write(platform, &analysis, prepared, false).await?;
            readiness::wait(platform, &written).await
        })
        .await
        .with_context(|| format!("resume application {application}: readiness budget expired"))?
        .with_context(|| format!("resume application {application}"))?;
    }
    let control = crate::platform::resources::release_values(
        &platform.target,
        &platform.target.namespace,
        &wamn_registry::name::control_resource(&document.org),
    )
    .await?
    .context("installed control host")?;
    ensure::provision_materializer(platform, &control, document).await?;
    Ok(())
}

async fn stop_group(platform: &Platform, name: &str) -> anyhow::Result<()> {
    use wamn_release_control::release_chart;
    if analyse::release_present(platform, name).await? {
        let bytes = crate::platform::resources::helm(
            &platform.target,
            &[
                "get".as_ref(),
                "values".as_ref(),
                name.as_ref(),
                "-n".as_ref(),
                platform.target.namespace.as_ref(),
                "-o".as_ref(),
                "json".as_ref(),
            ],
        )
        .await?;
        let mut values: Value = serde_json::from_slice(&bytes)?;
        stop_values(&mut values)?;
        let private = ensure::PrivateDirectory::create()?;
        let path = private.path("stopped.yaml");
        std::fs::write(&path, serde_yaml::to_string(&values)?)?;
        release_chart::recover_pending(&platform.target, name).await?;
        release_chart::upgrade(
            &platform.target,
            &platform.chart,
            name,
            &path,
            std::time::Duration::from_secs(120),
            "database reset",
        )
        .await?;
    }
    ensure!(
        super::super::drain::wait(platform, name, None)
            .await?
            .is_empty(),
        "database user {name} is still running"
    );
    Ok(())
}

/// Remove chart-owned workloads and stop their hosts using existing chart values.
/// Ordinary reapply derives the roles and host replicas from the selected release again.
pub fn stop_values(values: &mut Value) -> anyhow::Result<()> {
    values["roles"] = serde_json::json!([]);
    for group in values["runtime-operator"]["runtime"]["hostGroups"]
        .as_array_mut()
        .context("host groups")?
    {
        group["replicas"] = 0.into();
    }
    Ok(())
}

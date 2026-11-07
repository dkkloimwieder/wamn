//! Prepare a retained CNPG replacement without changing the serving database.

use std::collections::BTreeMap;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wamn_catalog::manifest::PackageManifest;
use wamn_package_control::{apply_package, database::connect};

use super::super::{Platform, analyse, document::EnvironmentDocument, ensure, stage};
use super::schema::{self, Candidate, SchemaSource};
use crate::provision_project_env::registry;

/// Immutable reconstruction inputs, not deployment progress.
#[derive(Debug, Serialize, Deserialize)]
struct Inputs {
    project_baseline: String,
    sources: BTreeMap<String, SchemaSource>,
}

/// The registry retains this identity across SQL failures and retries.
#[derive(Debug, Serialize, Deserialize)]
struct Pending {
    instance_suffix: String,
    inputs: Inputs,
}

impl Pending {
    fn database(&self, document: &EnvironmentDocument) -> String {
        wamn_control_provision::project_env_database_name(
            &document.org,
            &document.project,
            &document.env,
            &self.instance_suffix,
        )
    }
}

/// Resolve every retained schema and running release before reserving a target.
/// The caller holds the environment lifecycle lock.
pub async fn prepare(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: &str,
    release: &str,
    admin_url: &str,
) -> anyhow::Result<Value> {
    let (release, selected) =
        schema::release_candidates(platform, document, application, Some(release)).await?;
    let state = schema::state(platform, document).await?;
    ensure!(
        state.disposable == Some(true),
        "environment is not recorded as disposable"
    );
    let diff = schema::plan(document, application, Some(release), &selected, &state)?;
    let baseline = state
        .project_baseline
        .context("installed project substrate is unknown")?;
    ensure!(
        baseline == wamn_control_provision::schema_baseline::project_sha256(),
        "SQL image differs from installed project substrate"
    );
    let candidates = schema::reset_candidates(platform, &diff, selected).await?;
    validate_candidates(document, &candidates, &diff.reset_sources)?;
    resolve_releases(platform, document).await?;
    let inputs = serde_json::to_value(Inputs {
        project_baseline: baseline,
        sources: diff.reset_sources,
    })?;
    let mut system = connect(&platform.system_database_url).await?;
    let active = crate::provision_project_env::read_project_env_instance(
        &platform.system_database_url,
        &document.triple(),
    )
    .await?;
    ensure!(
        Some(database(document, &active).as_str()) == diff.database_instance.as_deref(),
        "active database changed during input resolution"
    );
    let pending: Pending = serde_json::from_value(
        registry::reserve_database(&mut system, &document.triple(), &active, &inputs).await?,
    )?;
    connect(admin_url)
        .await?
        .batch_execute(&crate::provision_project_env::role_posture_sql())
        .await?;
    let cluster = recorded_cluster(platform, document).await?;
    Ok(
        wamn_control_provision::database::render_project_env_database(
            &document.triple(),
            &pending.instance_suffix,
            &cluster,
            &platform.database_namespace,
            None,
        ),
    )
}

fn database(document: &EnvironmentDocument, instance: &str) -> String {
    wamn_control_provision::project_env_database_name(
        &document.org,
        &document.project,
        &document.env,
        instance,
    )
}

async fn recorded_cluster(
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
    let system = connect(&platform.system_database_url).await?;
    let row = analyse::read_row(&system, document)
        .await?
        .context("environment is not recorded")?;
    ensure!(row.disposable, "environment is not disposable");
    let pending: Pending = serde_json::from_value(
        registry::pending_database(&system, &document.triple())
            .await?
            .context("no pending database")?,
    )?;
    wamn_control_provision::validate_instance_suffix(&pending.instance_suffix)?;
    ensure!(
        pending.instance_suffix != row.instance_suffix,
        "pending database is active"
    );
    ensure!(
        pending.inputs.project_baseline
            == wamn_control_provision::schema_baseline::project_sha256(),
        "SQL image differs from installed project substrate"
    );
    Ok(pending)
}

fn validate_candidates(
    document: &EnvironmentDocument,
    candidates: &[Candidate],
    sources: &BTreeMap<String, SchemaSource>,
) -> anyhow::Result<(Vec<PackageManifest>, Vec<usize>)> {
    let mut packages = Vec::new();
    let mut found = std::collections::BTreeSet::new();
    for candidate in candidates {
        let manifest = PackageManifest::from_slice(&candidate.directory.manifest_bytes)?;
        let id = &manifest.package.id;
        ensure!(
            found.insert(id.as_str().to_owned()),
            "duplicate reset package {id}"
        );
        ensure!(
            document
                .applications
                .values()
                .any(|application| application.packages.contains(id)),
            "reset package {id} has no declared application"
        );
        for dependency in manifest.base_dependencies.values() {
            ensure!(
                sources.contains_key(&dependency.package),
                "reset package {id} is missing dependency {}",
                dependency.package
            );
        }
        let source = sources
            .get(id)
            .with_context(|| format!("unexpected reset package {id}"))?;
        ensure!(
            manifest.package.version == source.version
                && source.artifact.is_some()
                && source.artifact == candidate.root.artifact_digest
                && source.fingerprint == candidate.fingerprint,
            "reset package {id} differs from its immutable source"
        );
        // Includes filename/checksum validation before mutation. Ownership remains with apply-package.
        wamn_schema_control::plan_package_migrations(&candidate.directory, None)?;
        packages.push(manifest);
    }
    ensure!(
        found.len() == sources.len(),
        "incomplete reset package inputs"
    );
    let order = stage::apply_order(
        &packages
            .iter()
            .map(|p| {
                (
                    p.package.id.as_str(),
                    p.base_dependencies
                        .values()
                        .map(|d| d.package.as_str())
                        .collect(),
                )
            })
            .collect::<Vec<_>>(),
    )?;
    Ok((packages, order))
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
    let system = connect(&platform.system_database_url).await?;
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
            super::super::bindings::analyse(&system, &selected, &packages).await?;
            releases.push((application.clone(), digest.clone(), manifest, packages));
        }
    }
    Ok(releases)
}

/// Initialize only the recorded candidate after CNPG reports success.
/// Package SQL and execution records commit together through apply-package.
pub async fn initialize(
    platform: &Platform,
    document: &EnvironmentDocument,
    admin_url: &str,
) -> anyhow::Result<Value> {
    let pending = read_pending(platform, document).await?;
    let mut candidates = Vec::new();
    for (id, source) in &pending.inputs.sources {
        candidates
            .push(schema::open(platform, id, &source.version, source.artifact.as_deref()).await?);
    }
    let (packages, order) = validate_candidates(document, &candidates, &pending.inputs.sources)?;
    let releases = resolve_releases(platform, document).await?;
    let database = pending.database(document);
    let mut project_url = url::Url::parse(&platform.system_database_url)?;
    project_url.set_path(&database);
    let project_url = project_url.to_string();
    let admin = connect(admin_url).await?;
    admin
        .batch_execute(&crate::provision_project_env::privilege_sql(&database))
        .await?;
    let project_admin = ensure::ensure_database_at(
        platform,
        document,
        &packages,
        &admin,
        admin_url,
        &project_url,
        true,
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
    let env_key =
        wamn_control_provision::project_env_key(&document.org, &document.project, &document.env);
    for (application, digest, manifest, code_packages) in releases {
        stage::materialize_application_principals(
            &platform.system_database_url,
            &project_url,
            &env_key,
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
        )
        .await?;
    }
    let schemas = packages
        .iter()
        .flat_map(|package| package.models.values().map(|model| model.schema.clone()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let capture = wamn_control_provision::cdc_object_name(
        &document.org,
        &document.project,
        &document.env,
        &pending.instance_suffix,
    );
    if !schemas.is_empty() {
        // The candidate role is not published to serving workloads during preparation.
        admin
            .batch_execute(&wamn_control_provision::sql::ensure_replication_role_sql(
                &capture,
                &ensure::random_password()?,
            ))
            .await?;
        let project = connect(&project_admin).await?;
        for package in &packages {
            for model in package.models.values() {
                project
                    .batch_execute(&wamn_schema_control::alter_replica_identity_sql(
                        &model.schema,
                        &model.table,
                        wamn_schema_control::ReplicaIdentity::Full,
                    ))
                    .await?;
            }
        }
        project
            .batch_execute(&crate::enable_cdc_project_env::cdc_sql_bundle(
                &schemas, &capture, &database,
            ))
            .await?;
    }
    Ok(
        serde_json::json!({"database":database, "instance_suffix":pending.instance_suffix, "schemas":schemas, "capture":capture}),
    )
}

/// Prepare through the installed SQL image, then wait for CNPG's observed result.
pub async fn prepare_deployed(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: &str,
    release: &str,
) -> anyhow::Result<Value> {
    let private = ensure::PrivateDirectory::create()?;
    let chart = crate::platform::artifact_plan::select(&platform.chart)?;
    let cluster = recorded_cluster(platform, document).await?;
    let cr = super::run_deployed(
        platform,
        document,
        super::Operation::PrepareReset {
            application,
            release,
        },
        chart.image("ctl")?,
        &cluster,
        &private.path("prepare"),
    )
    .await?
    .context("missing candidate Database")?;
    crate::kubectl::apply(
        &platform.target.kubeconfig,
        &platform.target.context,
        &platform.database_namespace,
        &cr,
    )
    .await?;
    let name = cr["metadata"]["name"]
        .as_str()
        .context("candidate database name")?;
    ensure::wait_applied(platform, "database", name).await?;
    let result = super::run_deployed(
        platform,
        document,
        super::Operation::InitializeReset,
        chart.image("ctl")?,
        &cluster,
        &private.path("initialize"),
    )
    .await?
    .context("missing candidate initialization result")?;
    let schemas: Vec<String> = serde_json::from_value(result["schemas"].clone())?;
    if !schemas.is_empty() {
        let publication = wamn_control_provision::database::render_project_env_publication(
            &document.triple(),
            result["instance_suffix"]
                .as_str()
                .context("candidate instance")?,
            &cluster,
            &platform.database_namespace,
            result["capture"].as_str().context("candidate capture")?,
            &schemas,
        );
        crate::kubectl::apply(
            &platform.target.kubeconfig,
            &platform.target.context,
            &platform.database_namespace,
            &publication,
        )
        .await?;
        ensure::wait_applied(platform, "publication", name).await?;
    }
    Ok(result)
}

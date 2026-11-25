//! Project operations under the registry and project lifecycle locks.

mod rollback;

use super::database::{DatabaseConnection, LifecycleLock};
use crate::environment_sql_effects::{self, PostgresSession};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use wamn_application::application_config::APPLICATION_CONFIG;
use wamn_application::application_config::ApplicationConfig;
use wamn_package::PackageName;
use wamn_registry::ApplicationName;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::time::{Instant, timeout_at};
use wamn_component_policy::PackageManifest;
use wamn_digest::Digest;
use wamn_kubernetes::helm::{self, Wait};
use wamn_platform_install::release_chart::bundle::SelectedChart;
use wamn_platform_install::release_chart::{POD_REGISTRY, pod_registry_reference};
use wamn_registry::Triple;
use wamn_release::release_package::{OpenedPackage, open_package_digest};

use super::{
    Platform, bindings,
    document::{DeclaredRelease, EnvironmentDocument, WorkloadRelease},
    sql, stage, write,
};
use crate::apply_package::{ApplyPackageRequest, apply_package};

#[derive(Debug)]
pub enum Application<'a> {
    Deploy {
        digest: Digest,
        integration_base: &'a str,
        component_base: &'a str,
        chart: &'a SelectedChart,
        authority_port: write::AuthorityPort,
    },
    Apply(Digest),
    Diff(Option<Digest>),
    Rollback(Option<std::num::NonZeroU32>),
    Stop,
}

struct Packages {
    manifests: BTreeMap<PackageName, PackageManifest>,
    artifacts: BTreeMap<PackageName, OpenedPackage>,
}

impl Packages {
    async fn open(platform: &Platform, top: Digest) -> anyhow::Result<Self> {
        let base = platform
            .package_artifact_base
            .as_deref()
            .context("package artifact base is absent")?;
        let registry = platform.packages.repository(base).await?;
        let mut manifests = BTreeMap::new();
        let mut artifacts = BTreeMap::new();
        let mut pins = BTreeMap::new();
        let mut pending = vec![top];
        let mut visited = BTreeSet::new();
        while let Some(digest) = pending.pop() {
            if !visited.insert(digest) {
                continue;
            }
            let (artifact, manifest) = open_package_digest(&registry, &digest).await?;
            let pins_by_alias = &manifest
                .deploy
                .as_ref()
                .context("package has no compiled deploy section")?
                .base_packages;
            ensure!(
                pins_by_alias.keys().eq(manifest.base_dependencies.keys()),
                "package {} has inconsistent base pins",
                manifest.package.id
            );
            pending.extend(pins_by_alias.values().copied());
            let id = manifest.package.id.clone();
            ensure!(
                !manifests.contains_key(&id),
                "package closure selects two digests for {id}"
            );
            pins.insert(id.clone(), digest);
            artifacts.insert(id.clone(), artifact);
            manifests.insert(id, manifest);
        }
        for manifest in manifests.values() {
            let bases = &manifest
                .deploy
                .as_ref()
                .expect("verified deploy section")
                .base_packages;
            for (alias, base) in &manifest.base_dependencies {
                ensure!(
                    pins.get(&base.package) == bases.get(alias)
                        && manifests[&base.package].package.version == base.version,
                    "package {} pins the wrong artifact for base {}",
                    manifest.package.id,
                    base.package
                );
            }
        }
        Ok(Self {
            manifests,
            artifacts,
        })
    }

    fn order(&self) -> anyhow::Result<Vec<&OpenedPackage>> {
        let bases = self
            .manifests
            .iter()
            .map(|(id, manifest)| {
                (
                    id,
                    manifest
                        .base_dependencies
                        .values()
                        .map(|base| &base.package)
                        .collect(),
                )
            })
            .collect::<Vec<_>>();
        Ok(stage::apply_order(&bases)?
            .into_iter()
            .map(|index| &self.artifacts[bases[index].0])
            .collect())
    }
}

async fn operation_job_deadline(platform: &Platform) -> anyhow::Result<Instant> {
    let job = platform.cluster.current_job().await?;
    let start = job["status"]["startTime"]
        .as_str()
        .context("application operation Job start time is absent")?;
    let start =
        chrono::DateTime::parse_from_rfc3339(start).context("invalid operation Job start time")?;
    let budget = job["spec"]["activeDeadlineSeconds"]
        .as_i64()
        .and_then(chrono::TimeDelta::try_seconds)
        .context("invalid operation Job activeDeadlineSeconds")?;
    let deadline = start
        .checked_add_signed(budget)
        .context("invalid operation Job deadline")?;
    let now = Instant::now();
    let remaining = (deadline.to_utc() - chrono::Utc::now())
        .to_std()
        .context("operation Job deadline elapsed")?;
    now.checked_add(remaining)
        .context("invalid operation Job deadline")
}

async fn recover_release(
    platform: &Platform,
    document: &EnvironmentDocument,
    release: &str,
) -> anyhow::Result<(Instant, Duration)> {
    let deadline = operation_job_deadline(platform).await?;
    let readiness = Duration::from_secs(u64::try_from(document.readiness_budget_seconds)?);
    let namespace = helm::org_release_namespace(&platform.cluster.namespace);
    let wait = Wait::WatcherUntil {
        deadline,
        readiness,
    };
    timeout_at(
        deadline,
        helm::recover_pending(&platform.cluster, &namespace, release, wait),
    )
    .await
    .with_context(|| {
        format!("operation Job deadline reached recovering {namespace}/{release}")
    })??;
    Ok((deadline, readiness))
}

/// Execute an application operation using its applied project document.
pub async fn application(
    platform: &Platform,
    mut document: EnvironmentDocument,
    name: &ApplicationName,
    operation: Application<'_>,
    journal_release: &mut Option<WorkloadRelease>,
) -> anyhow::Result<Value> {
    let triple = document.triple();
    Box::pin(LifecycleLock::hold(
        DatabaseConnection::Direct(&platform.system_database_url),
        &triple,
        async |system| {
            ensure!(
                document.applications.contains_key(name),
                "application {name} is not declared"
            );
            let connection = system.connection(&triple).await?;
            platform.prepare_database(&connection).await?;
            let owner = connection.to_json()?;
            let release = wamn_registry::resource_names::application_release_name(
                &triple.org,
                &triple.project,
                &triple.env,
                name,
            );
            let connection_mode = if matches!(operation, Application::Deploy { .. }) {
                DatabaseConnection::Wait {
                    input: &owner,
                    database: &connection.database,
                }
            } else {
                DatabaseConnection::Direct(&owner)
            };
            LifecycleLock::hold(connection_mode, &triple, async |project| match operation {
                Application::Deploy {
                    digest,
                    integration_base,
                    component_base,
                    chart,
                    authority_port,
                } => {
                    let (deadline, readiness) =
                        recover_release(platform, &document, &release).await?;
                    let packages = Packages::open(platform, digest).await?;
                    let selection = document
                        .applications
                        .get_mut(name)
                        .expect("declared application");
                    selection.current = DeclaredRelease::Digest(digest);
                    selection.packages = packages.manifests.keys().cloned().collect();
                    let platform_domain = system.platform_domain().await?;
                    let bindings =
                        bindings::analyse(system, project, &document, packages.manifests.values())
                            .await?;
                    let mut values = write::release_values(
                        &document,
                        &platform_domain,
                        name,
                        &packages.manifests,
                        &digest,
                        &bindings,
                        integration_base,
                        component_base,
                        authority_port,
                    )?;
                    use_pod_registry(chart, &mut values)?;
                    for image in write::workload_images(&values, chart)? {
                        let (authority, _) = image
                            .split_once('/')
                            .with_context(|| format!("image {image} is missing its registry"))?;
                        let insecure_registry = authority == POD_REGISTRY
                            || wamn_registry::registry_is_loopback(authority);
                        platform
                            .packages
                            .require_image(image, insecure_registry)
                            .await?;
                    }
                    let app = application_config(&values)?;
                    let sequences = stage::sequence_defaults(
                        &document.membership(),
                        packages.manifests.values(),
                    )?;
                    project.check_workflows(&app).await?;
                    apply_sql(platform, system, &triple, &connection, &owner, &packages).await?;
                    environment_sql_effects::materialize_application_principals(
                        &platform_domain,
                        &owner,
                        &wamn_registry::project_env_key(&triple.org, &triple.project, &triple.env),
                        &packages.manifests,
                    )
                    .await?;
                    project.create_number_sequences(&sequences).await?;
                    wamn_platform_install::deployment::apply_application(
                        &platform.cluster,
                        chart.directory(),
                        &release,
                        &values,
                        deadline,
                        readiness,
                    )
                    .await?;
                    match super::document::workload_release(&platform.cluster, &triple, name).await {
                        Ok(release) => *journal_release = release.filter(|release| release.package_digest == digest),
                        Err(error) => tracing::warn!(%error, "read the deployed revision for the platform journal"),
                    }
                    Ok(json!({"release":digest}))
                }
                Application::Apply(digest) => {
                    let packages = Packages::open(platform, digest).await?;
                    apply_sql(platform, system, &triple, &connection, &owner, &packages).await?;
                    Ok(json!({"release":digest}))
                }
                Application::Diff(digest) => {
                    let digest = if let Some(digest) = digest {
                        digest
                    } else {
                        super::document::workload_release(&platform.cluster, &triple, name)
                            .await?
                            .context("application has no deployed package")?
                            .package_digest
                    };
                    let packages = Packages::open(platform, digest).await?;
                    let candidates = packages
                        .order()?
                        .into_iter()
                        .map(|artifact| sql::schema::Candidate::source(artifact.package_root()))
                        .collect::<Result<Vec<_>, _>>()?;
                    let key =
                        wamn_registry::project_env_key(&triple.org, &triple.project, &triple.env);
                    let state = sql::schema::database_state(&owner, &key).await?;
                    let diff =
                        sql::schema::plan(&document, name, Some(digest), &candidates, &state)
                            .await?;
                    Ok(serde_json::to_value(diff)?)
                }
                Application::Rollback(revision) => {
                    let (deadline, readiness) =
                        recover_release(platform, &document, &release).await?;
                    let revision =
                        rollback::target(platform, &triple, name, &release, revision).await?;
                    let (digest, app) = rollback::application(platform, &release, revision).await?;
                    *journal_release = Some(WorkloadRelease { package_digest: digest, revision });
                    let packages = Packages::open(platform, digest).await?;
                    rollback::verify_package_components(&app, &packages.manifests)?;
                    project.check_workflows(&app).await?;
                    rollback::run(platform, &release, revision, deadline, readiness).await?;
                    Ok(json!({"release":digest,"revision":revision}))
                }
                Application::Stop => {
                    super::drain::stop(platform, project, &triple.env, name, &release).await?;
                    Ok(json!({"stopped":name}))
                }
            })
            .await
        },
    ))
    .await
}

/// Apply the pod-local registry references for an application deployment.
pub fn use_pod_registry(
    chart: &SelectedChart,
    values: &mut serde_yaml::Value,
) -> anyhow::Result<()> {
    if let Cow::Owned(image) = pod_registry_reference(chart.image("ingress")?) {
        values["platform"]["roles"]["ingress"]["image"] = image.into();
    }
    if let Cow::Owned(image) = pod_registry_reference(chart.image("workflow")?) {
        values["platform"]["workflow"]["image"] = image.into();
    }
    for component in values["components"]
        .as_sequence_mut()
        .expect("release values contain a component list")
    {
        if let serde_yaml::Value::String(image) = &mut component["image"]
            && let Cow::Owned(pod_image) = pod_registry_reference(image)
        {
            *image = pod_image;
        }
    }
    Ok(())
}

fn application_config(values: &serde_yaml::Value) -> anyhow::Result<ApplicationConfig> {
    let text = values["route"]["config"][APPLICATION_CONFIG]
        .as_str()
        .context("release has no wamn.application configuration")?;
    Ok(serde_json::from_str(text)?)
}

async fn apply_sql(
    platform: &Platform,
    system: &PostgresSession,
    triple: &Triple,
    connection: &wamn_postgres::database::ConnectionConfig,
    owner: &str,
    packages: &Packages,
) -> anyhow::Result<()> {
    let order = packages.order()?;
    let schemas = publication_schemas(platform, owner, &packages.manifests).await?;
    let migration =
        wamn_postgres::migration_scope::MigrationDatabase::Project.configuration(owner)?;
    for artifact in order {
        apply_package(ApplyPackageRequest {
            package: artifact.package_root(),
            database_url: owner.into(),
            migration_database_url: migration.clone(),
            org: triple.org.clone(),
            project: triple.project.clone(),
            environment: triple.env.clone(),
        })
        .await?;
    }
    let cluster = connection
        .server_ca
        .name
        .strip_suffix("-ca")
        .context("project cluster CA")?;
    let instance = system.instance(triple).await?;
    let publication =
        wamn_registry::name::cdc_object_name(&triple.org, &triple.project, &triple.env, &instance);
    let mut values =
        super::chart_values::publication_values(triple, &instance, cluster, &publication, &schemas);
    let paused = system.has_capture_gap(triple).await?;
    values["cdcReader"] = json!({"pause":paused});
    let coord =
        wamn_registry::name::environment_resource_name(&triple.org, &triple.project, &triple.env);
    wamn_platform_install::deployment::update_project(
        &platform.cluster,
        &platform.chart,
        &coord,
        values,
    )
    .await?;
    if schemas.is_empty() {
        return Ok(());
    }
    let capture = wamn_postgres::database::capture_configuration(connection.clone())?;
    platform.prepare_database(&capture).await?;
    environment_sql_effects::prepare_cdc(
        owner,
        &capture.to_json()?,
        &connection.database,
        &publication,
        &schemas,
    )
    .await?;
    if !paused {
        platform.cluster.wait_reader_ready(triple).await?;
    }
    Ok(())
}

async fn publication_schemas(
    platform: &Platform,
    owner: &str,
    selected: &BTreeMap<PackageName, PackageManifest>,
) -> anyhow::Result<Vec<wamn_package::canonical_name::CanonicalName>> {
    let mut schemas = selected
        .values()
        .flat_map(|package| {
            package
                .models
                .values()
                .map(|model| model.schema.clone().into())
        })
        .collect::<BTreeSet<_>>();
    let installed = environment_sql_effects::installed_packages(owner)
        .await?
        .into_iter()
        .filter(|(id, _)| !selected.contains_key(id));
    for (_, _, manifest) in stage::open(platform, installed).await? {
        schemas.extend(
            manifest
                .models
                .into_values()
                .map(|model| model.schema.into()),
        );
    }
    Ok(schemas.into_iter().collect())
}

/// Reapply the installed SQL of a disposable project under its lifecycle lock.
pub async fn reset(platform: &Platform, document: &EnvironmentDocument) -> anyhow::Result<Value> {
    let triple = document.triple();
    // Reset has no application selection. Its SQL application sources are the
    // exact installed artifact digests of the whole database.
    Box::pin(LifecycleLock::hold(
        DatabaseConnection::Direct(&platform.system_database_url),
        &triple,
        async |system| {
            let disposable = system.disposable(&triple).await?;
            ensure!(disposable, "environment is not recorded as disposable");
            let conn = system.connection(&triple).await?;
            platform.prepare_database(&conn).await?;
            let owner = conn.to_json()?;
            LifecycleLock::hold(
                DatabaseConnection::Direct(&owner),
                &triple,
                async |project| {
                    let unfinished = project.unfinished(&triple.env, None).await?;
                    ensure!(
                        unfinished.is_empty(),
                        "{}",
                        super::drain::describe(&unfinished)
                    );
                    let installed = project.installed_packages().await?;
                    let mut packages = Packages {
                        manifests: BTreeMap::new(),
                        artifacts: BTreeMap::new(),
                    };
                    for (id, artifact, manifest) in stage::open(platform, installed).await? {
                        packages.manifests.insert(id.clone(), manifest);
                        packages.artifacts.insert(id, artifact);
                    }
                    packages.order()?;
                    super::ensure::suspend_project(platform, system, &triple, None).await?;
                    let migration = wamn_postgres::migration_scope::MigrationDatabase::Project
                        .configuration(&owner)?;
                    system.reset(&owner, &migration, &triple).await?;
                    apply_sql(platform, system, &triple, &conn, &owner, &packages).await?;
                    let values = super::ensure::read_host_values(
                        platform,
                        system,
                        &triple.org,
                        &triple.env,
                        &[],
                        None,
                        None,
                    )
                    .await?;
                    super::ensure::upgrade_host(platform, &triple.org, &triple.env, &values)
                        .await?;
                    Ok(json!({"reset":triple.project}))
                },
            )
            .await
        },
    ))
    .await
}

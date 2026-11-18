//! Project operations under the registry and project lifecycle locks.

mod rollback;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_catalog::{
    ApplicationName, PackageName, WiringDocument,
    application_config::{APPLICATION_CONFIG, ApplicationConfig},
    lifecycle_lock::LifecycleLock,
    manifest::PackageManifest,
};
use wamn_digest::Digest;
use wamn_publish::package_artifact::{OpenedPackage, open_package_digest};
use wamn_registry::Triple;

use super::{
    Platform, bindings, capture_gap,
    document::{DeclaredRelease, EnvironmentDocument},
    ensure as resources, sql, stage, write,
};
use crate::apply_package::{ApplyPackageRequest, apply_package};

#[derive(Debug)]
pub enum Application<'a> {
    Deploy {
        digest: Digest,
        integration_base: &'a str,
        component_base: &'a str,
        actor: &'a wamn_identity_protocol::PrincipalId,
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
        let registry = platform.registry(base).await?;
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

/// Execute an application operation using its applied project document.
pub async fn application(
    platform: &Platform,
    mut document: EnvironmentDocument,
    name: &ApplicationName,
    operation: Application<'_>,
) -> anyhow::Result<Value> {
    let triple = document.triple();
    LifecycleLock::hold(
        wamn_postgres::connect(&platform.system_database_url).await?,
        &triple,
        async |system| {
            ensure!(
                document.applications.contains_key(name),
                "application {name} is not declared"
            );
            let connection = wamn_platform_install::environment::connection(
                &platform.system_database_url,
                &triple,
            )
            .await?;
            let owner = connection.to_json()?;
            let release = wamn_catalog::resource_names::application_release_name(
                &triple.org,
                &triple.project,
                &triple.env,
                name,
            );
            let project = if matches!(operation, Application::Deploy { .. }) {
                wamn_platform_install::environment::wait_project_connection(
                    &owner,
                    &connection.database,
                )
                .await?
            } else {
                wamn_postgres::connect(&owner).await?
            };
            LifecycleLock::hold(
                project,
                &triple,
                async |project| match operation {
                    Application::Deploy {
                        digest,
                        integration_base,
                        component_base,
                        actor,
                    } => {
                        let packages = Packages::open(platform, digest).await?;
                        let selection = document
                            .applications
                            .get_mut(name)
                            .expect("declared application");
                        selection.current = DeclaredRelease::Digest(digest);
                        selection.packages = packages.manifests.keys().cloned().collect();
                        let mut system_client = system.client().await;
                        let mut project_client = project.client().await;
                        let bindings = bindings::analyse(
                            &mut system_client,
                            &mut project_client,
                            &document,
                            packages.manifests.values(),
                        )
                        .await?;
                        let (values, _) = write::release_values(
                            &document,
                            name,
                            &packages.manifests,
                            &digest,
                            &bindings,
                            integration_base,
                            &actor.to_string(),
                            component_base,
                        )?;
                        let app = application_config(&values)?;
                        check_workflows(&mut project_client, &app).await?;
                        drop(project_client);
                        drop(system_client);
                        apply_sql(platform, system, &triple, &connection, &owner, &packages)
                            .await?;
                        stage::materialize_application_principals(
                            platform,
                            &owner,
                            &wamn_registry::project_env_key(
                                &triple.org,
                                &triple.project,
                                &triple.env,
                            ),
                            &packages.manifests,
                        )
                        .await?;
                        wamn_platform_install::environment::apply_application(
                            &platform.target,
                            &platform.chart,
                            &release,
                            &values,
                        )
                        .await?;
                        Ok(json!({"release":digest}))
                    }
                    Application::Apply(digest) => {
                        let packages = Packages::open(platform, digest).await?;
                        apply_sql(platform, system, &triple, &connection, &owner, &packages)
                            .await?;
                        Ok(json!({"release":digest}))
                    }
                    Application::Diff(digest) => {
                        let digest = if let Some(digest) = digest {
                            digest
                        } else {
                            let api = wamn_kubernetes::kubernetes_api::KubernetesApi::in_cluster()?;
                            super::document::workload_release(&api, &triple, name)
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
                        let key = wamn_registry::project_env_key(
                            &triple.org,
                            &triple.project,
                            &triple.env,
                        );
                        let state = sql::schema::database_state(&owner, &key).await?;
                        let diff =
                            sql::schema::plan(&document, name, Some(digest), &candidates, &state)
                                .await?;
                        Ok(serde_json::to_value(diff)?)
                    }
                    Application::Rollback(revision) => {
                        let revision =
                            rollback::target(platform, &triple, name, &release, revision).await?;
                        let (digest, app) =
                            rollback::application(platform, &release, revision).await?;
                        let packages = Packages::open(platform, digest).await?;
                        rollback::verify_package_components(&app, &packages.manifests)?;
                        check_workflows(&mut *project.client().await, &app).await?;
                        rollback::run(platform, &release, revision).await?;
                        Ok(json!({"release":digest,"revision":revision}))
                    }
                    Application::Stop => {
                        let unfinished = super::drain::unfinished(
                            &mut *project.client().await,
                            &triple.env,
                            Some(name),
                        )
                        .await?;
                        ensure!(
                            unfinished.is_empty(),
                            "{}",
                            super::drain::describe(&unfinished)
                        );
                        use wamn_kubernetes::helm::{self, Wait};
                        helm::uninstall(&platform.target, &release, Wait::Watcher).await?;
                        Ok(json!({"stopped":name}))
                    }
                },
            )
            .await
        },
    )
    .await
}

fn application_config(values: &serde_yaml::Value) -> anyhow::Result<ApplicationConfig> {
    let text = values["roles"][0]["config"][APPLICATION_CONFIG]
        .as_str()
        .context("release has no wamn.application configuration")?;
    Ok(serde_json::from_str(text)?)
}

async fn check_workflows(
    project: &mut sqlx::PgConnection,
    app: &ApplicationConfig,
) -> anyhow::Result<()> {
    let graphs = sqlx::query_as::<_, (String, Value)>(
        "SELECT workflow_id, graph_json FROM catalog.workflows
         WHERE environment=$1 AND application=$2 AND enabled ORDER BY workflow_id",
    )
    .bind(&app.environment)
    .bind(&app.application)
    .fetch_all(project)
    .await?;
    for (name, value) in graphs {
        let graph = WiringDocument::parse(&value)?;
        wamn_catalog::application_config::check_workflow_references(app, &graph)
            .with_context(|| format!("enabled workflow {name}"))?;
    }
    Ok(())
}

async fn apply_sql(
    platform: &Platform,
    system: &LifecycleLock,
    triple: &Triple,
    connection: &wamn_postgres::ConnectionConfig,
    owner: &str,
    packages: &Packages,
) -> anyhow::Result<()> {
    let order = packages.order()?;
    let schemas = publication_schemas(platform, owner, &packages.manifests).await?;
    let migration = wamn_provision::migrations::MigrationDatabase::Project.configuration(owner)?;
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
    let instance = wamn_registry::deployment::instance_suffix(&mut *system.client().await, triple)
        .await?
        .context("project deployment instance is absent")?;
    let publication =
        wamn_provision::cdc_object_name(&triple.org, &triple.project, &triple.env, &instance);
    let mut values = wamn_platform_install::environment::publication_values(
        triple,
        &instance,
        cluster,
        &publication,
        &schemas,
    );
    let paused = capture_gap::is_open(&mut *system.client().await, triple).await?;
    values["cdcReader"] = json!({"pause":paused});
    let coord =
        wamn_registry::name::environment_resource_name(&triple.org, &triple.project, &triple.env);
    wamn_platform_install::environment::update_project(
        &platform.target,
        &platform.chart,
        &coord,
        &values,
    )
    .await?;
    let capture = wamn_platform_install::environment::capture_connection(
        &platform.system_database_url,
        triple,
    )
    .await?;
    resources::prepare_cdc(
        owner,
        &capture,
        &connection.database,
        &publication,
        &schemas,
    )
    .await?;
    if !paused {
        super::cdc_reader::wait_reader_ready(&platform.target, triple).await?;
    }
    Ok(())
}

async fn publication_schemas(
    platform: &Platform,
    owner: &str,
    selected: &BTreeMap<PackageName, PackageManifest>,
) -> anyhow::Result<Vec<wamn_catalog::canonical_name::CanonicalName>> {
    let mut schemas = selected
        .values()
        .flat_map(|package| {
            package
                .models
                .values()
                .map(|model| model.schema.clone().into())
        })
        .collect::<BTreeSet<_>>();
    let mut project = wamn_postgres::connect(owner).await?;
    let installed = sqlx::query_as::<_, (PackageName, Option<Digest>)>(
        "SELECT package_id, artifact_digest FROM catalog.packages",
    )
    .fetch_all(&mut project)
    .await?;
    let base = platform
        .package_artifact_base
        .as_deref()
        .context("package artifact base is absent")?;
    let registry = platform.registry(base).await?;
    for (id, digest) in installed {
        if selected.contains_key(&id) {
            continue;
        }
        let digest =
            digest.with_context(|| format!("installed package {id} requires a pinned artifact"))?;
        let (_, manifest) = open_package_digest(&registry, &digest).await?;
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
    LifecycleLock::hold(
        wamn_postgres::connect(&platform.system_database_url).await?,
        &triple,
        async |system| {
            let mut client = system.client().await;
            let disposable = wamn_registry::deployment::disposable(&mut client, &triple).await?;
            ensure!(disposable, "environment is not recorded as disposable");
            drop(client);
            let conn = wamn_platform_install::environment::connection(
                &platform.system_database_url,
                &triple,
            )
            .await?;
            let owner = conn.to_json()?;
            LifecycleLock::hold(
                wamn_postgres::connect(&owner).await?,
                &triple,
                async |project| {
                    let mut client = project.client().await;
                    let unfinished =
                        super::drain::unfinished(&mut client, &triple.env, None).await?;
                    ensure!(
                        unfinished.is_empty(),
                        "{}",
                        super::drain::describe(&unfinished)
                    );
                    let installed = sqlx::query_as::<_, (PackageName, Option<Digest>)>(
                        "SELECT package_id, artifact_digest FROM catalog.packages",
                    )
                    .fetch_all(&mut *client)
                    .await?;
                    drop(client);
                    let base = platform
                        .package_artifact_base
                        .as_deref()
                        .context("package artifact base is absent")?;
                    let registry = platform.registry(base).await?;
                    let mut packages = Packages {
                        manifests: BTreeMap::new(),
                        artifacts: BTreeMap::new(),
                    };
                    for (id, digest) in installed {
                        let digest = digest.with_context(|| {
                            format!("installed package {id} requires a pinned artifact")
                        })?;
                        let (artifact, manifest) = open_package_digest(&registry, &digest).await?;
                        ensure!(
                            manifest.package.id == id,
                            "installed artifact {digest} is not {id}"
                        );
                        packages.manifests.insert(id.clone(), manifest);
                        packages.artifacts.insert(id, artifact);
                    }
                    packages.order()?;
                    super::ensure::suspend_project(platform, &triple).await?;
                    let migration = wamn_provision::migrations::MigrationDatabase::Project
                        .configuration(&owner)?;
                    sql::reset::reset_database(
                        &platform.system_database_url,
                        &owner,
                        &migration,
                        &triple,
                    )
                    .await?;
                    apply_sql(platform, system, &triple, &conn, &owner, &packages).await?;
                    let (_, host) = super::ensure::prepare_host_upgrade(
                        platform,
                        &triple.org,
                        &triple.env,
                        &document.policy,
                        &[],
                    )
                    .await?;
                    host.await?;
                    Ok(json!({"reset":triple.project}))
                },
            )
            .await
        },
    )
    .await
}

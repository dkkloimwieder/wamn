//! Current selections and unfinished-run releases share one native host.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Context as _;
use tokio::sync::{Mutex, watch};
use wamn_broker::Publisher;
use wamn_catalog::{ApplicationDefinition, ServingManifest};
use wamn_engine::artifact_source::ArtifactSource;
use wamn_engine::release_manifest::{LoadedRelease, ReleaseTarget};
use wamn_execution_host::{
    HostRouteDelivery, HostRouteHandlers, OperationHost, OperationHostConfig,
};
use wamn_kubernetes::kubernetes_api::KubernetesApi;
use wamn_runtime::connection_facts::ConnectionFacts;
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::{WamnJetstream, WamnPostgres};
use wamn_workflow::{QueueService, QueueServiceConfig, RouterDriver};
use wash_runtime::engine::Engine;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::wasi_logging::TracingLogger;

type ReleaseKey = String;

pub(super) struct Resources {
    pub engine: Arc<Engine>,
    pub postgres: Arc<WamnPostgres>,
    pub transport: Arc<HttpTransport>,
    pub facts: Arc<ConnectionFacts>,
    pub logging: Arc<TracingLogger>,
    pub source: Arc<dyn ArtifactSource>,
    pub allowed_hosts: Arc<[AllowedHost]>,
    pub configuration: OperationHostConfig,
    pub target: ReleaseTarget,
    pub org: String,
    pub connection_reader: Option<sqlx::PgPool>,
    pub identity: Option<(sqlx::PgPool, String)>,
    pub administration_url: Option<String>,
    pub changes: Publisher,
    pub jetstream: Arc<WamnJetstream>,
    pub registry: Option<(
        wamn_package_artifact::PackageRegistry,
        wamn_package_artifact::registry_credentials::RegistryCredentialSource,
    )>,
    pub queue_lease_ttl_ms: u64,
}

pub(super) struct ResidentRelease {
    pub operations: Arc<OperationHost>,
    pub handlers: Arc<HostRouteDelivery>,
    pub files: Arc<wamn_execution_host::file_route::FileRoutes>,
    queue: Arc<QueueService>,
    stop: watch::Sender<bool>,
}

struct Selection {
    host: String,
    digest: String,
    release: watch::Sender<Option<Arc<LoadedRelease>>>,
}

pub(super) struct Releases {
    resources: Resources,
    api: KubernetesApi,
    resident: RwLock<BTreeMap<ReleaseKey, Arc<ResidentRelease>>>,
    selections: RwLock<BTreeMap<String, Selection>>,
    loading: Mutex<()>,
}

impl Releases {
    pub fn new(resources: Resources, api: KubernetesApi) -> Self {
        Self {
            resources,
            api,
            resident: RwLock::default(),
            selections: RwLock::default(),
            loading: Mutex::new(()),
        }
    }

    /// Retain the selected deployment through the workload's stop and active calls.
    pub async fn for_workload(
        &self,
        annotations: &std::collections::HashMap<String, String>,
    ) -> anyhow::Result<Arc<ResidentRelease>> {
        let digest = annotations
            .get("wamn.release-digest")
            .context("application workload has no release digest")?;
        let application = annotations
            .get("wamn.application")
            .context("application workload has no application")?;
        anyhow::ensure!(
            annotations.get("wamn.project_env_key") == Some(&self.resources.target.project_env_key),
            "workload belongs to another project deployment"
        );
        self.load(application, digest).await
    }

    pub fn files(&self, host: &str) -> Option<Arc<wamn_execution_host::file_route::FileRoutes>> {
        let selections = self
            .selections
            .read()
            .expect("release selections lock poisoned");
        let (_, selection) = selections
            .iter()
            .find(|(_, selection)| selection.host == host)?;
        let resident = self
            .resident
            .read()
            .expect("resident releases lock poisoned");
        Some(Arc::clone(&resident.get(&selection.digest)?.files))
    }

    async fn load(&self, application: &str, digest: &str) -> anyhow::Result<Arc<ResidentRelease>> {
        let _loading = self.loading.lock().await;
        if let Some(resident) = self
            .resident
            .read()
            .expect("resident releases lock poisoned")
            .get(digest)
        {
            anyhow::ensure!(
                resident
                    .operations
                    .release
                    .target()
                    .applications
                    .contains_key(application),
                "application is outside the loaded deployment"
            );
            return Ok(Arc::clone(resident));
        }
        let r = &self.resources;
        let bytes = r
            .postgres
            .release_bytes(&r.configuration.project, &r.target.project_env_key, digest)
            .await?;
        let (manifest, _) = ServingManifest::from_canonical_bytes(&bytes)?;
        anyhow::ensure!(
            manifest.digest().as_str() == digest,
            "release catalog digest mismatch"
        );
        let packages = manifest
            .release
            .packages
            .iter()
            .map(|package| package.package_id().to_owned())
            .collect();
        let applications = BTreeMap::from([(
            application.to_owned(),
            ApplicationDefinition {
                packages,
                ..ApplicationDefinition::default()
            },
        )]);
        let target = ReleaseTarget::new(&r.target.project_env_key, &r.target.environment)
            .with_applications(applications);
        let release = Arc::new(LoadedRelease::load_canonical_bytes(&bytes, digest, target)?);
        let operations = Arc::new(OperationHost::new(
            Arc::clone(&r.engine),
            Arc::clone(&r.postgres),
            Arc::clone(&r.transport),
            Arc::clone(&r.facts),
            Arc::clone(&r.logging),
            Arc::clone(&r.allowed_hosts),
            Arc::clone(&release),
            Arc::clone(&r.source),
            r.configuration.clone(),
        )?);
        if let Some((registry, credentials)) = &r.registry {
            operations.set_integrations(Arc::new(
                wamn_runtime::integrations::Integrations::new(
                    Arc::clone(&r.facts),
                    &release,
                    &r.postgres,
                    &r.configuration.project,
                    registry.clone(),
                    credentials.clone(),
                )
                .await?,
            ))?;
        }
        let connections = Arc::new(wamn_execution_host::file_route::RegistryFileConnections {
            client: r.connection_reader.clone(),
            release: Arc::clone(&release),
            facts: Arc::clone(&r.facts),
            org: r.org.clone(),
            project: r.configuration.project.clone(),
            environment: r.target.environment.clone(),
        });
        let files = Arc::new(wamn_execution_host::file_route::FileRoutes::new(
            Arc::clone(&operations.context),
            Arc::clone(&r.postgres),
            Arc::new(
                wamn_runtime::plugins::wamn_blobstore::plugin::WamnBlobstore::new(Arc::clone(
                    &r.postgres,
                )),
            ),
            connections,
            r.configuration.project.clone(),
            r.administration_url.clone(),
        ));
        operations.set_file_routes(Arc::clone(&files))?;
        wamn_execution_host::prepare_release(&operations).await?;
        let handlers = Arc::new(
            HostRouteDelivery::new(
                Arc::clone(&release),
                HostRouteHandlers::Application {
                    postgres: Arc::clone(&r.postgres),
                    project: r.configuration.project.clone(),
                    identity: r.identity.clone(),
                    changes: r.changes.clone(),
                },
            )
            .with_files(Some(Arc::clone(&files))),
        );
        let queue = Arc::new(
            QueueService::bind(
                Arc::new(RouterDriver::new(Arc::clone(&operations))),
                Arc::clone(&r.postgres),
                Arc::clone(&r.jetstream),
                &release,
                QueueServiceConfig {
                    project: r.configuration.project.clone(),
                    runner: r.configuration.owner_prefix.clone(),
                    lease_ttl_ms: r.queue_lease_ttl_ms,
                },
            )
            .await?,
        );
        let resident = Arc::new(ResidentRelease {
            operations,
            handlers,
            files,
            queue,
            stop: watch::channel(false).0,
        });
        self.resident
            .write()
            .expect("resident releases lock poisoned")
            .insert(digest.to_owned(), Arc::clone(&resident));
        Ok(resident)
    }

    async fn selected(&self) -> anyhow::Result<BTreeMap<String, (String, String)>> {
        let r = &self.resources;
        let maps = self
            .api
            .get(format!(
                "/api/v1/namespaces/{}/configmaps",
                self.api.namespace
            ))
            .await?;
        let mut selected = BTreeMap::new();
        for map in maps["items"]
            .as_array()
            .context("ConfigMap list has no items")?
        {
            let annotations = &map["metadata"]["annotations"];
            let Some(application) = annotations["wamn.application"].as_str() else {
                continue;
            };
            if annotations["wamn.environment"].as_str()
                != Some(
                    wamn_registry::name::host_status(
                        &r.org,
                        &r.configuration.project,
                        &r.target.environment,
                    )
                    .as_str(),
                )
            {
                continue;
            }
            let projection = (|| -> anyhow::Result<(String, String)> {
                let json = map["data"]["release.json"]
                    .as_str()
                    .context("release projection is absent")?;
                let projection: serde_json::Value = serde_json::from_str(json)?;
                anyhow::ensure!(
                    projection["org"].as_str() == Some(&r.org)
                        && projection["project"].as_str() == Some(&r.configuration.project),
                    "release projection belongs to another deployment"
                );
                let digest = projection["digest"]
                    .as_str()
                    .context("release projection has no digest")?;
                anyhow::ensure!(
                    annotations["wamn.release-digest"].as_str() == Some(digest),
                    "release projection digest differs"
                );
                let host = projection["host"]
                    .as_str()
                    .context("release projection has no host")?;
                Ok((digest.to_owned(), host.to_owned()))
            })();
            match projection {
                Ok(projection) => {
                    anyhow::ensure!(
                        selected
                            .insert(application.to_owned(), projection)
                            .is_none(),
                        "application has multiple current selections"
                    );
                }
                Err(error) => {
                    tracing::warn!(%application, %error, "application selection is unusable");
                }
            }
        }
        Ok(selected)
    }

    async fn refresh(&self) -> anyhow::Result<()> {
        let r = &self.resources;
        let pins = r
            .postgres
            .unfinished_releases(
                &r.configuration.project,
                &r.target.project_env_key,
                &r.target.environment,
            )
            .await?;
        let selected = match self.selected().await {
            Ok(selected) => selected,
            Err(error) => {
                tracing::warn!(%error, "current selections unavailable; admission paused");
                BTreeMap::new()
            }
        };
        let needed: BTreeSet<_> = selected
            .iter()
            .map(|(application, (digest, _))| (application.as_str(), digest.as_str()))
            .chain(
                pins.iter()
                    .map(|(application, digest, _)| (application.as_str(), digest.as_str())),
            )
            .collect();
        {
            let mut selections = self
                .selections
                .write()
                .expect("release selections lock poisoned");
            for (application, selection) in selections.iter_mut() {
                if !selected.contains_key(application) {
                    selection.release.send_replace(None);
                    selection.host.clear();
                    selection.digest.clear();
                }
            }
            for (application, (digest, host)) in &selected {
                if !selections.contains_key(application) {
                    selections.insert(
                        application.clone(),
                        Selection {
                            host: host.clone(),
                            digest: digest.clone(),
                            release: watch::channel(None).0,
                        },
                    );
                }
                let selection = selections
                    .get_mut(application)
                    .expect("selection was inserted");
                if selection.digest != *digest {
                    selection.release.send_replace(None);
                }
                selection.digest.clone_from(digest);
                selection.host.clone_from(host);
            }
        }
        for (application, digest) in &needed {
            match self.load(application, digest).await {
                Ok(resident) => {
                    if selected
                        .get(*application)
                        .is_some_and(|(current, _)| current.as_str() == *digest)
                    {
                        self.selections
                            .read()
                            .expect("release selections lock poisoned")[*application]
                            .release
                            .send_replace(Some(Arc::clone(&resident.operations.release)));
                    }
                }
                Err(error) => {
                    let runs: Vec<_> = pins
                        .iter()
                        .filter(|(app, pin, _)| app == *application && pin == *digest)
                        .map(|(_, _, run)| run.as_str())
                        .collect();
                    tracing::warn!(%application, %digest, ?runs, error = %format_args!("{error:#}"), "release preparation failed; other releases remain available");
                }
            }
        }
        let needed_digests: BTreeSet<_> = needed.iter().map(|(_, digest)| *digest).collect();
        self.resident
            .write()
            .expect("resident releases lock poisoned")
            .retain(|key, resident| {
                let keep = needed_digests.contains(key.as_str())
                    || Arc::strong_count(resident) > 1
                    || r.postgres.release_in_use(&resident.operations.release);
                if !keep {
                    resident.stop.send_replace(true);
                }
                keep
            });
        Ok(())
    }

    /// Reconcile residency while each release drains its own queue independently.
    pub async fn serve(&self, mut stopping: watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut queues = tokio::task::JoinSet::new();
        let mut queue_tasks = BTreeSet::new();
        let mut admissions = tokio::task::JoinSet::new();
        let mut admission_tasks = BTreeSet::new();
        let mut ticks = tokio::time::interval(Duration::from_secs(1));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = stopping.changed() => break,
                _ = ticks.tick() => {},
            }
            let refreshed = tokio::select! {
                biased;
                _ = stopping.changed() => break,
                refreshed = self.refresh() => refreshed,
            };
            if let Err(error) = refreshed {
                tracing::warn!(error = %format_args!("{error:#}"), "release reconciliation failed");
            }
            for (key, resident) in self
                .resident
                .read()
                .expect("resident releases lock poisoned")
                .iter()
            {
                if queue_tasks.insert(key.clone()) {
                    let queue = Arc::clone(&resident.queue);
                    let stopping = resident.stop.subscribe();
                    let key = key.clone();
                    queues.spawn(async move { (key, queue.serve(stopping).await) });
                }
            }
            while let Some(result) = queues.try_join_next() {
                match result {
                    Ok((key, result)) => {
                        queue_tasks.remove(&key);
                        if let Err(error) = result {
                            tracing::warn!(%error, "release queue stopped");
                        }
                    }
                    Err(error) => tracing::warn!(%error, "release queue task failed"),
                }
            }
            for (application, selection) in self
                .selections
                .read()
                .expect("release selections lock poisoned")
                .iter()
            {
                if !admission_tasks.insert(application.clone()) {
                    continue;
                }
                let rows = wamn_workflow::schedule::PostgresSchedules::new(
                    Arc::clone(&self.resources.postgres),
                    &self.resources.configuration.project,
                    application,
                    selection.release.subscribe(),
                );
                let schedule_stopping = stopping.clone();
                admissions.spawn(async move {
                    let _ = wamn_workflow::schedule::serve(&rows, schedule_stopping).await;
                });
                let postgres = Arc::clone(&self.resources.postgres);
                let events = Arc::clone(&self.resources.jetstream);
                let project = self.resources.configuration.project.clone();
                let application = application.clone();
                let selection = selection.release.subscribe();
                let stopped = stopping.clone();
                admissions.spawn(super::serve_application_events(
                    selection,
                    stopped,
                    async move |release: &Arc<LoadedRelease>| {
                        if let Err(error) =
                            wamn_workflow::event_admission::admit_application_events(
                                &events,
                                &postgres,
                                &project,
                                &application,
                                release,
                            )
                            .await
                        {
                            tracing::warn!(%error, "application event admission failed");
                        }
                    },
                ));
            }
        }
        admissions.shutdown().await;
        for resident in self
            .resident
            .read()
            .expect("resident releases lock poisoned")
            .values()
        {
            resident.stop.send_replace(true);
        }
        while let Some(result) = queues.join_next().await {
            result?.1?;
        }
        Ok(())
    }
}

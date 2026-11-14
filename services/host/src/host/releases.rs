//! Current selections and unfinished-run releases share one native host.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Context as _;
use tokio::sync::{Mutex, watch};
use wamn_broker::Publisher;
use wamn_catalog::{ApplicationDefinition, ServingManifest};
use wamn_connections::connection_facts::ConnectionFacts;
use wamn_engine::artifact_source::ArtifactSource;
use wamn_engine::release_manifest::{LoadedRelease, ReleaseTarget};
use wamn_events_capability::WamnJetstream;
use wamn_execution_host::{
    HostRouteDelivery, HostRouteHandlers, OperationHost, OperationHostConfig,
};
use wamn_http_capability::transport::HttpTransport;
use wamn_kubernetes::kubernetes_api::KubernetesApi;
use wamn_postgres_capability::WamnPostgres;
use wamn_workflow::{QueueService, QueueServiceConfig, RouterDriver};
use wash_runtime::engine::Engine;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::wasi_logging::TracingLogger;

pub(super) type ManifestCache =
    Arc<std::sync::Mutex<BTreeMap<String, Arc<tokio::sync::OnceCell<Arc<[u8]>>>>>>;

type ReleaseKey = (String, String);

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
    pub leases: Arc<wamn_connections::resource_lease::ResourceLeases>,
}

pub(super) struct ResidentRelease {
    pub operations: Arc<OperationHost>,
    pub handlers: Arc<HostRouteDelivery>,
    pub files: Arc<wamn_execution_host::file_route::FileRoutes>,
    queue: Arc<QueueService>,
    _manifest: Arc<[u8]>,
    stop: watch::Sender<bool>,
}

struct Selection {
    host: String,
    digest: String,
    release: watch::Sender<Option<Arc<LoadedRelease>>>,
}

pub(super) struct Releases {
    resources: Resources,
    paused: std::sync::atomic::AtomicBool,
    api: KubernetesApi,
    resident: RwLock<BTreeMap<ReleaseKey, Arc<ResidentRelease>>>,
    selections: RwLock<BTreeMap<String, Selection>>,
    loading: std::sync::Mutex<BTreeMap<ReleaseKey, Arc<Mutex<()>>>>,
    bytes: ManifestCache,
}

impl Releases {
    pub fn new(resources: Resources, api: KubernetesApi, bytes: ManifestCache) -> Self {
        Self {
            resources,
            paused: std::sync::atomic::AtomicBool::new(false),
            api,
            resident: RwLock::default(),
            selections: RwLock::default(),
            loading: std::sync::Mutex::default(),
            bytes,
        }
    }

    /// Stop new schedule and event admissions while existing runs and calls finish.
    pub fn pause_admission(&self) {
        self.paused
            .store(true, std::sync::atomic::Ordering::Release);
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
        let (application, selection) = selections
            .iter()
            .find(|(_, selection)| selection.host == host)?;
        let resident = self
            .resident
            .read()
            .expect("resident releases lock poisoned");
        Some(Arc::clone(
            &resident
                .get(&(application.clone(), selection.digest.clone()))?
                .files,
        ))
    }

    async fn load(&self, application: &str, digest: &str) -> anyhow::Result<Arc<ResidentRelease>> {
        let key = (application.to_owned(), digest.to_owned());
        let loading = {
            let mut loading = self
                .loading
                .lock()
                .expect("release preparations lock poisoned");
            Arc::clone(loading.entry(key.clone()).or_default())
        };
        let _loading = loading.lock().await;
        if let Some(resident) = self
            .resident
            .read()
            .expect("resident releases lock poisoned")
            .get(&key)
        {
            return Ok(Arc::clone(resident));
        }
        let r = &self.resources;
        let cached = {
            let mut bytes = self.bytes.lock().expect("release bytes lock poisoned");
            Arc::clone(bytes.entry(digest.to_owned()).or_default())
        };
        let authorized = r
            .postgres
            .release_bytes(&r.configuration.project, &r.target.project_env_key, digest)
            .await?;
        let (manifest, _) = ServingManifest::from_canonical_bytes(&authorized)?;
        anyhow::ensure!(
            manifest.digest().as_str() == digest,
            "release catalog digest mismatch"
        );
        let bytes = cached
            .get_or_init(|| async { Arc::<[u8]>::from(authorized) })
            .await;
        let (manifest, _) = ServingManifest::from_canonical_bytes(bytes)?;
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
                wamn_integrations::Integrations::new(
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
            Arc::new(wamn_blobstore::plugin::WamnBlobstore::new(Arc::clone(
                &r.postgres,
            ))),
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
            _manifest: Arc::clone(bytes),
            stop: watch::channel(false).0,
        });
        self.resident
            .write()
            .expect("resident releases lock poisoned")
            .insert(key, Arc::clone(&resident));
        Ok(resident)
    }

    async fn selected(&self) -> anyhow::Result<BTreeMap<String, (String, String)>> {
        if self.paused.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(BTreeMap::new());
        }
        let r = &self.resources;
        let maps = self
            .api
            .get(format!(
                "/api/v1/namespaces/{}/configmaps",
                self.api.namespace
            ))
            .await?;
        let mut selected = BTreeMap::new();
        let mut ambiguous = BTreeSet::new();
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
                    if ambiguous.contains(application) {
                        continue;
                    }
                    if selected.contains_key(application) {
                        selected.remove(application);
                        ambiguous.insert(application.to_owned());
                        tracing::warn!(%application, "application has multiple current selections; its admission is paused");
                    } else {
                        selected.insert(application.to_owned(), projection);
                    }
                }
                Err(error) => {
                    tracing::warn!(%application, %error, "application selection is unusable");
                }
            }
        }
        Ok(selected)
    }

    async fn refresh(&self) -> anyhow::Result<BTreeMap<ReleaseKey, Vec<String>>> {
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
        let mut needed: BTreeMap<ReleaseKey, Vec<String>> = selected
            .iter()
            .map(|(application, (digest, _))| ((application.clone(), digest.clone()), Vec::new()))
            .collect();
        for (application, digest, run) in pins {
            needed.entry((application, digest)).or_default().push(run);
        }
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
        self.resident
            .write()
            .expect("resident releases lock poisoned")
            .retain(|key, resident| {
                let keep = needed.contains_key(key)
                    || Arc::strong_count(resident) > 1
                    || r.postgres.release_in_use(&resident.operations.release);
                if !keep {
                    resident.stop.send_replace(true);
                }
                keep
            });
        self.loading
            .lock()
            .expect("release preparations lock poisoned")
            .retain(|key, lock| needed.contains_key(key) || Arc::strong_count(lock) > 1);
        Ok(needed)
    }

    /// Read current residency and unfinished-run pins from the owning project.
    pub async fn residency(
        &self,
    ) -> anyhow::Result<Vec<wamn_kubernetes::environment_state::ReleaseResidency>> {
        let r = &self.resources;
        let pins = r
            .postgres
            .unfinished_releases(
                &r.configuration.project,
                &r.target.project_env_key,
                &r.target.environment,
            )
            .await?;
        let mut releases: BTreeMap<
            ReleaseKey,
            wamn_kubernetes::environment_state::ReleaseResidency,
        > = self
            .resident
            .read()
            .expect("resident releases lock poisoned")
            .keys()
            .map(|key| {
                (
                    key.clone(),
                    wamn_kubernetes::environment_state::ReleaseResidency {
                        application: key.0.clone(),
                        manifest_digest: key.1.clone(),
                        loaded: true,
                        runs: Vec::new(),
                    },
                )
            })
            .collect();
        for (application, selection) in self
            .selections
            .read()
            .expect("release selections lock poisoned")
            .iter()
            .filter(|(_, selection)| !selection.digest.is_empty())
        {
            releases
                .entry((application.clone(), selection.digest.clone()))
                .or_insert_with(|| wamn_kubernetes::environment_state::ReleaseResidency {
                    application: application.clone(),
                    manifest_digest: selection.digest.clone(),
                    loaded: false,
                    runs: Vec::new(),
                });
        }
        for (application, digest, run) in pins {
            releases
                .entry((application.clone(), digest.clone()))
                .or_insert_with(|| wamn_kubernetes::environment_state::ReleaseResidency {
                    application,
                    manifest_digest: digest,
                    loaded: false,
                    runs: Vec::new(),
                })
                .runs
                .push(run);
        }
        Ok(releases.into_values().collect())
    }

    /// Reconcile residency while each release drains its own queue independently.
    pub async fn serve(
        self: &Arc<Self>,
        mut stopping: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let mut preparations =
            tokio::task::JoinSet::<(ReleaseKey, anyhow::Result<Arc<ResidentRelease>>)>::new();
        let mut preparing = BTreeSet::new();
        let mut needed = BTreeMap::<ReleaseKey, Vec<String>>::new();
        let mut collectors = tokio::task::JoinSet::new();
        let mut collector_tasks = BTreeSet::new();
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
                Some(prepared) = preparations.join_next(), if !preparations.is_empty() => {
                    let (key, result) = prepared?;
                    preparing.remove(&key);
                    match result {
                        Ok(resident) => {
                            let selections = self.selections.read().expect("release selections lock poisoned");
                            if let Some(selection) = selections.get(&key.0).filter(|selection| selection.digest == key.1) {
                                selection.release.send_replace(Some(Arc::clone(&resident.operations.release)));
                            }
                        }
                        Err(error) => {
                            let runs = needed.get(&key);
                            tracing::warn!(application = %key.0, digest = %key.1, ?runs, error = %format_args!("{error:#}"), "release preparation failed; other releases remain available");
                        }
                    }
                }
                _ = ticks.tick() => {
                    let refreshed = tokio::select! {
                        biased;
                        _ = stopping.changed() => break,
                        refreshed = self.refresh() => refreshed,
                    };
                    match refreshed {
                        Ok(refreshed) => needed = refreshed,
                        Err(error) => tracing::warn!(error = %format_args!("{error:#}"), "release reconciliation failed"),
                    }
                    for key in needed.keys() {
                        let loaded = self.resident.read().expect("resident releases lock poisoned").get(key).map(Arc::clone);
                        if let Some(resident) = loaded {
                            if let Some(selection) = self.selections.read().expect("release selections lock poisoned").get(&key.0).filter(|selection| selection.digest == key.1) {
                                selection.release.send_replace(Some(Arc::clone(&resident.operations.release)));
                            }
                        } else if preparing.insert(key.clone()) {
                            let releases = Arc::clone(self);
                            let key = key.clone();
                            preparations.spawn(async move {
                                let result = releases.load(&key.0, &key.1).await;
                                (key, result)
                            });
                        }
                    }
                }
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
                if collector_tasks.insert(key.clone()) {
                    let files = Arc::clone(&resident.files);
                    let stopped = resident.stop.subscribe();
                    let key = key.clone();
                    collectors.spawn(async move {
                        collect_files(files, stopped).await;
                        key
                    });
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
            while let Some(result) = collectors.try_join_next() {
                match result {
                    Ok(key) => {
                        collector_tasks.remove(&key);
                    }
                    Err(error) => tracing::warn!(%error, "file collector task failed"),
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
        preparations.shutdown().await;
        admissions.shutdown().await;
        collectors.shutdown().await;
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
        self.resources.leases.shutdown().await;
        Ok(())
    }
}

async fn collect_files(
    files: Arc<wamn_execution_host::file_route::FileRoutes>,
    mut stopped: watch::Receiver<bool>,
) {
    let mut ticks = tokio::time::interval(wamn_execution_host::file_collector::COLLECTION_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = stopped.changed() => return,
            _ = ticks.tick() => {},
        }
        tokio::select! {
            biased;
            _ = stopped.changed() => return,
            collected = files.collect_orphan_files() => {
                if let Err(error) = collected { tracing::warn!(%error, "host file orphan collection failed"); }
            }
        }
    }
}

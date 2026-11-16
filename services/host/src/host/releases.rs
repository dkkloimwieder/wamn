use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use super::projects::StatePoisoned;
use anyhow::Context as _;
use tokio::sync::{Mutex, watch};
use wamn_broker::Publisher;
use wamn_catalog::{ApplicationDefinition, ServingManifest};
use wamn_connections::connection_facts::ConnectionFacts;
use wamn_engine::artifact_source::ArtifactSource;
use wamn_engine::release_manifest::{LoadedRelease, ReleaseTarget};
use wamn_execution_host::{
    HostRouteDelivery, HostRouteHandlers, OperationHost, OperationHostConfig,
};
use wamn_http::transport::HttpTransport;
use wamn_sql::WamnPostgres;
use wash_runtime::engine::Engine;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::wasi_logging::TracingLogger;

// Entries are independent cells. Initialization happens outside the map lock,
// and insertion or eviction cannot invalidate another digest's bytes. Every
// load still authorizes and verifies the bytes before using the shared cell.
pub(super) type ManifestCache =
    Arc<std::sync::Mutex<BTreeMap<wamn_digest::Digest, Arc<tokio::sync::OnceCell<Arc<[u8]>>>>>>;

type ReleaseKey = (wamn_catalog::ApplicationName, wamn_digest::Digest);

pub(super) struct Resources {
    pub engine: Arc<Engine>,
    pub postgres: Arc<WamnPostgres>,
    pub blobstore: Arc<wamn_blobstore::plugin::WamnBlobstore>,
    pub transport: Arc<HttpTransport>,
    pub facts: Arc<ConnectionFacts>,
    pub logging: Arc<TracingLogger>,
    pub source: Arc<dyn ArtifactSource>,
    pub allowed_hosts: Arc<[AllowedHost]>,
    pub configuration: OperationHostConfig,
    pub target: ReleaseTarget,
    pub org: wamn_registry::OrgId,
    pub connection_reader: Option<sqlx::PgPool>,
    pub identity: Option<(sqlx::PgPool, wamn_registry::OrgId)>,
    pub administration_url: Option<String>,
    pub changes: Publisher,
    pub registry: Option<(
        wamn_package_artifact::PackageRegistry,
        wamn_package_artifact::registry_credentials::RegistryCredentialSource,
    )>,
    pub leases: Arc<wamn_connections::resource_lease::ResourceLeases>,
}

pub(super) struct ResidentRelease {
    pub operations: Arc<OperationHost>,
    pub handlers: Arc<HostRouteDelivery>,
    pub files: Arc<wamn_execution_host::file_route::FileRoutes>,
    _manifest: Arc<[u8]>,
    stop: watch::Sender<bool>,
}

pub(super) struct Releases {
    resources: Resources,
    resident: RwLock<BTreeMap<ReleaseKey, Arc<ResidentRelease>>>,
    loading: std::sync::Mutex<BTreeMap<ReleaseKey, Arc<Mutex<()>>>>,
    bytes: ManifestCache,
}

impl Releases {
    pub fn new(resources: Resources, bytes: ManifestCache) -> Self {
        Self {
            resources,
            resident: RwLock::default(),
            loading: std::sync::Mutex::default(),
            bytes,
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
            annotations.get("wamn.project_env_key").map(String::as_str)
                == Some(self.resources.target.project_env_key.as_str()),
            "workload belongs to another project deployment"
        );
        self.load(&application.parse()?, &digest.parse()?).await
    }

    async fn load(
        &self,
        application: &wamn_catalog::ApplicationName,
        digest: &wamn_digest::Digest,
    ) -> anyhow::Result<Arc<ResidentRelease>> {
        let key = (application.clone(), digest.to_owned());
        let loading = {
            let mut loading = self
                .loading
                .lock()
                .map_err(|_| StatePoisoned("release preparations"))?;
            Arc::clone(loading.entry(key.clone()).or_default())
        };
        let _loading = loading.lock().await;
        if let Some(resident) = self
            .resident
            .read()
            .map_err(|_| StatePoisoned("resident releases"))?
            .get(&key)
        {
            return Ok(Arc::clone(resident));
        }
        let r = &self.resources;
        let cached = {
            // Only the map is recovered; each cell initializes independently.
            let mut bytes = self.bytes.lock().unwrap_or_else(|error| error.into_inner());
            Arc::clone(bytes.entry(digest.to_owned()).or_default())
        };
        let authorized = wamn_workflow::run_queue::release_bytes(
            &r.postgres,
            &r.configuration.project,
            &r.target.project_env_key,
            digest,
        )
        .await?;
        let (manifest, _) = ServingManifest::from_canonical_bytes(&authorized)?;
        anyhow::ensure!(
            manifest.digest() == *digest,
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
            application.clone(),
            ApplicationDefinition {
                packages,
                ..ApplicationDefinition::default()
            },
        )]);
        let target = ReleaseTarget::new(
            r.target.project_env_key.clone(),
            r.target.environment.clone(),
        )
        .with_applications(applications);
        let release = Arc::new(LoadedRelease::load_canonical_bytes(
            &bytes,
            &digest.to_string(),
            target,
        )?);
        let operations = Arc::new(OperationHost::new(
            Arc::clone(&r.engine),
            Arc::clone(&r.postgres),
            Arc::clone(&r.blobstore),
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
                wamn_connections::integrations::Integrations::new(
                    Arc::clone(&r.facts),
                    &release,
                    r.postgres.as_ref(),
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
            Arc::clone(&r.blobstore),
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
        let resident = Arc::new(ResidentRelease {
            operations,
            handlers,
            files,
            _manifest: Arc::clone(bytes),
            stop: watch::channel(false).0,
        });
        self.resident
            .write()
            .map_err(|_| StatePoisoned("resident releases"))?
            .insert(key, Arc::clone(&resident));
        Ok(resident)
    }

    pub async fn serve(
        self: &Arc<Self>,
        mut stopping: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let mut collectors = tokio::task::JoinSet::new();
        let mut collector_tasks = BTreeSet::new();
        let mut ticks = tokio::time::interval(Duration::from_secs(1));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = stopping.changed() => break,
                _ = ticks.tick() => {},
            }
            for (key, resident) in self
                .resident
                .read()
                .map_err(|_| StatePoisoned("resident releases"))?
                .iter()
            {
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
            while let Some(result) = collectors.try_join_next() {
                match result {
                    Ok(key) => {
                        collector_tasks.remove(&key);
                    }
                    Err(error) => tracing::warn!(%error, "file collector task failed"),
                }
            }
        }
        collectors.shutdown().await;
        for resident in self
            .resident
            .read()
            .map_err(|_| StatePoisoned("resident releases"))?
            .values()
        {
            resident.stop.send_replace(true);
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

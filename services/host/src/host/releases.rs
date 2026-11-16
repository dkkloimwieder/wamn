use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use super::projects::StatePoisoned;
use anyhow::Context as _;
use tokio::sync::Mutex;
use wamn_broker::Publisher;
use wamn_catalog::{ApplicationDefinition, ServingManifest};
use wamn_connections::connection_facts::ConnectionFacts;
use wamn_engine::release_manifest::{LoadedRelease, ReleaseTarget};
use wamn_execution_host::{
    HostRouteDelivery, HostRouteHandlers, OperationHost, OperationHostConfig,
};
use wamn_sql::WamnPostgres;

// Entries are independent cells. Initialization happens outside the map lock,
// and insertion or eviction cannot invalidate another digest's bytes. Every
// load still authorizes and verifies the bytes before using the shared cell.
pub(super) type ManifestCache =
    Arc<std::sync::Mutex<BTreeMap<wamn_digest::Digest, Arc<tokio::sync::OnceCell<Arc<[u8]>>>>>>;

type ReleaseKey = (wamn_catalog::ApplicationName, wamn_digest::Digest);

pub(super) struct Resources {
    pub postgres: Arc<WamnPostgres>,
    pub blobstore: Arc<wamn_blobstore::plugin::WamnBlobstore>,
    pub facts: Arc<ConnectionFacts>,
    pub configuration: OperationHostConfig,
    pub target: ReleaseTarget,
    pub org: wamn_registry::OrgId,
    pub connection_reader: Option<sqlx::PgPool>,
    pub identity: Option<(sqlx::PgPool, wamn_registry::OrgId)>,
    pub administration_url: Option<String>,
    pub changes: Publisher,
}

pub(super) struct ResidentRelease {
    pub operations: Arc<OperationHost>,
    pub handlers: Arc<HostRouteDelivery>,
    _manifest: Arc<[u8]>,
}

pub(super) struct Releases {
    resources: Resources,
    resident: RwLock<BTreeMap<ReleaseKey, Arc<ResidentRelease>>>,
    loading: std::sync::Mutex<BTreeMap<ReleaseKey, Arc<Mutex<()>>>>,
    bytes: ManifestCache,
}

impl Releases {

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
            Arc::clone(&r.facts),
            Arc::clone(&release),
            r.configuration.clone(),
        )?);
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
            _manifest: Arc::clone(bytes),
        });
        self.resident
            .write()
            .map_err(|_| StatePoisoned("resident releases"))?
            .insert(key, Arc::clone(&resident));
        Ok(resident)
    }

}

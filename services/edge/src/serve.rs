//! The edge host: one release bundle served behind local routes.
//!
//! A plain wash-runtime host, with no cluster host and no NATS, holds the
//! engine, the two route plugins over the edge authenticator and delivery, and
//! an ingress. The route guest runs as its own workload, built from the bundle
//! bytes. The application loads once as a native application.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use wamn_engine::engine::build_engine;
use wamn_engine::expected_router::expected_host_router;
use wamn_engine::flow_http_routing::{FlowHttpRouting, RouteInFlightLimit};
use wamn_engine::router_delivery::RouterDelivery;
use wamn_run_state_sqlite::SqliteIntentStore;
use wamn_session::file_keys::FileKeys;
use wamn_session::verifier::SessionVerifier;
use wash_runtime::host::http::Ingress;
use wash_runtime::host::{Host, HostApi as _, HostBuilder};
use wash_runtime::types::{
    Component, LocalResources, Workload, WorkloadStartRequest, WorkloadState,
};
use wash_runtime::wit::WitInterface;

use crate::application::EdgeApplication;
use crate::authenticator::EdgeAuthenticator;
use crate::delivery::EdgeDelivery;
use crate::release::{EdgeRelease, file_digest};

/// The workload that runs the route guest.
const INGRESS_WORKLOAD: &str = "flow-http";

/// Everything the box needs to serve, read from the environment.
#[derive(Debug, Clone)]
pub struct EdgeConfig {
    /// The release bundle directory.
    pub bundle_dir: PathBuf,
    /// The pinned digest of `edge-release.json`.
    pub bundle_digest: String,
    /// The session key file that the release installs.
    pub session_keys: PathBuf,
    /// The session issuer that signs the keys.
    pub session_issuer: String,
    /// The organization a session must name.
    pub session_org: String,
    /// The project-environment identity a session must name.
    pub session_audience: String,
    /// The SQLite run-state file.
    pub db: PathBuf,
    /// The route host that the ingress answers.
    pub route_host: String,
    /// The address the ingress listens on.
    pub listen: SocketAddr,
}

impl EdgeConfig {
    /// Read every setting from its `WAMN_EDGE_*` variable.
    pub fn from_env() -> anyhow::Result<Self> {
        let var = |name: &str| std::env::var(name).with_context(|| format!("read {name}"));
        Ok(Self {
            bundle_dir: var("WAMN_EDGE_BUNDLE_DIR")?.into(),
            bundle_digest: var("WAMN_EDGE_BUNDLE_DIGEST")?,
            session_keys: var("WAMN_EDGE_SESSION_KEYS")?.into(),
            session_issuer: var("WAMN_EDGE_SESSION_ISSUER")?,
            session_org: var("WAMN_EDGE_SESSION_ORG")?,
            session_audience: var("WAMN_EDGE_SESSION_AUDIENCE")?,
            db: var("WAMN_EDGE_DB")?.into(),
            route_host: var("WAMN_EDGE_ROUTE_HOST")?,
            listen: var("WAMN_EDGE_LISTEN")?
                .parse()
                .context("parse WAMN_EDGE_LISTEN")?,
        })
    }
}

/// A started edge host.
#[derive(Debug)]
pub struct EdgeHost {
    host: Arc<Host>,
    addr: SocketAddr,
    stopping: tokio::sync::watch::Sender<bool>,
}

impl EdgeHost {
    /// The address the ingress is bound to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Refuse new requests, then stop the host and its workloads.
    pub async fn stop(self) -> anyhow::Result<()> {
        let _ = self.stopping.send(true);
        self.host.stop().await
    }
}

/// Load the pinned release and serve its routes.
pub async fn serve(config: EdgeConfig) -> anyhow::Result<EdgeHost> {
    let release = Arc::new(
        EdgeRelease::load(&config.bundle_dir, &config.bundle_digest)
            .await
            .context("load the release bundle")?,
    );
    let engine = Arc::new(build_engine(&[]).context("build the engine")?);
    let keys = FileKeys::load(&config.session_keys, &config.session_issuer)
        .context("load the session key file")?;
    let verifier = SessionVerifier::new(keys, &config.session_org, &config.session_audience)
        .context("bind the session scope")?;
    let intents = SqliteIntentStore::open(&config.db).context("open the run-state file")?;
    let application = EdgeApplication::load(Arc::clone(&engine), &release)
        .await
        .context("load the release application")?;

    let routing = FlowHttpRouting::new(
        Some(Arc::clone(release.release())),
        RouteInFlightLimit::default(),
    )
    .with_authenticator(Arc::new(EdgeAuthenticator::new(
        verifier,
        Arc::clone(&release),
    )));
    let delivery = RouterDelivery::new(Arc::new(EdgeDelivery::new(
        Arc::clone(&release),
        application,
        intents,
        config.session_org.clone(),
    )));

    let (stopping, stopped) = tokio::sync::watch::channel(false);
    let ingress = Arc::new(
        Ingress::builder(
            expected_host_router(Some(release.release()), stopped),
            config.listen,
        )
        .build()
        .await
        .context("bind the ingress")?,
    );
    let addr = ingress.addr();
    let host = HostBuilder::default()
        .with_engine((*engine).clone())
        .with_plugin(Arc::new(routing))?
        .with_plugin(Arc::new(delivery))?
        .with_http_handler(ingress)
        .build()
        .context("build the host")?
        .start()
        .await
        .context("start the host")?;

    let started = host
        .workload_start(ingress_workload(&release, &config.route_host))
        .await
        .context("start the route guest")?;
    if started.workload_status.workload_state != WorkloadState::Running {
        let status = started.workload_status;
        let _ = stopping.send(true);
        host.stop().await?;
        anyhow::bail!("the route guest did not start: {status:?}");
    }
    tracing::info!(
        bundle_digest = release.bundle_digest(),
        %addr,
        "wamn-edge serves its release"
    );
    Ok(EdgeHost {
        host,
        addr,
        stopping,
    })
}

/// The route guest's workload, with its bytes from the bundle.
fn ingress_workload(release: &EdgeRelease, route_host: &str) -> WorkloadStartRequest {
    let mut handler = WitInterface::from("wasi:http/handler@0.3.0");
    handler
        .config
        .insert("host".to_owned(), route_host.to_owned());
    let manifest = release.release().manifest();
    WorkloadStartRequest {
        workload_id: INGRESS_WORKLOAD.to_owned(),
        workload: Workload {
            namespace: manifest.release.environment.clone(),
            name: INGRESS_WORKLOAD.to_owned(),
            annotations: HashMap::new(),
            service: None,
            components: vec![Component {
                name: INGRESS_WORKLOAD.to_owned(),
                bytes: release.ingress().to_vec().into(),
                digest: Some(file_digest(release.ingress())),
                local_resources: LocalResources::default(),
                pool_size: 0,
                max_invocations: 0,
                max_concurrency: 0,
                reclaim_window_seconds: 0,
                reclaim_min_instances: 0,
            }],
            host_interfaces: vec![
                handler,
                WitInterface::from("wamn:flow-http-routing/routing@0.1.0"),
                WitInterface::from("wamn:router-delivery/delivery@0.1.0"),
            ],
            volumes: Vec::new(),
        },
    }
}

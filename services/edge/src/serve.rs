//! The edge host: one release bundle served behind local routes.
//!
//! A plain wash-runtime host, with no cluster host and no NATS, holds the
//! engine, the two route plugins over the edge authenticator and delivery, and
//! an ingress. The route guest runs as its own workload, built from the bundle
//! bytes. The application loads once as a native application. When the
//! configuration names a device, the device loop calls the same delivery, and
//! when it names a forward, the forward sends each stored sample on.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context as _;
use wamn_catalog::edge_bundle::file_digest;
use wamn_engine::engine::build_engine;
use wamn_engine::expected_router::expected_host_router;
use wamn_engine::flow_http_routing::{FlowHttpRouting, RouteInFlightLimit};
use wamn_engine::router_delivery::RouterDelivery;
use wamn_run_state_sqlite::{SqliteIntentStore, StoreClosed};
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
use crate::config::EdgeConfig;
use crate::delivery::EdgeDelivery;
use crate::device::{self, DeviceLoop};
use crate::forward::{self, Forward};
use crate::release::EdgeRelease;
use crate::samples::SampleStore;

/// The workload that runs the route guest.
const INGRESS_WORKLOAD: &str = "flow-http";

/// A started edge host.
#[derive(Debug)]
pub struct EdgeHost {
    host: Arc<Host>,
    addr: SocketAddr,
    stopping: tokio::sync::watch::Sender<bool>,
    device: Option<DeviceLoop>,
    forward: Option<Forward>,
    samples: SampleStore,
    /// Resolves when the last handle of the run-state file drops.
    closed: StoreClosed,
}

impl EdgeHost {
    /// The address the ingress is bound to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The samples of the device loop.
    pub fn samples(&self) -> &SampleStore {
        &self.samples
    }

    /// The device loop, when the configuration names a device.
    pub fn device(&self) -> Option<&DeviceLoop> {
        self.device.as_ref()
    }

    /// The forward, when the configuration names one.
    pub fn forward(&self) -> Option<&Forward> {
        self.forward.as_ref()
    }

    /// Refuse new requests and frames, let the device call in flight finish,
    /// drop a forward in flight, then stop the host and its workloads. The
    /// platform key makes the repeat of a dropped forward harmless. Returns
    /// after the run-state file closes, so another process can open it.
    pub async fn stop(self) -> anyhow::Result<()> {
        let Self {
            host,
            stopping,
            device,
            forward,
            samples,
            closed,
            ..
        } = self;
        let _ = stopping.send(true);
        if let Some(device) = device {
            device.join().await;
        }
        if let Some(forward) = forward {
            forward.join().await;
        }
        let stopped = host.stop().await;
        // A request task of the stopped host can hold the delivery, and with
        // it the store, a moment longer.
        drop(samples);
        closed.wait().await;
        stopped
    }
}

/// Load the pinned release, serve its routes, and run the device loop when
/// the configuration names a device.
pub async fn serve(config: EdgeConfig) -> anyhow::Result<EdgeHost> {
    let release = Arc::new(
        EdgeRelease::load(&config.release.dir, &config.release.digest)
            .await
            .context("load the release bundle")?,
    );
    let engine = Arc::new(build_engine(&[]).context("build the engine")?);
    let keys = FileKeys::load(&config.session.keys, &config.session.issuer)
        .context("load the session key file")?;
    let verifier = SessionVerifier::new(keys, &config.session.org, &config.session.audience)
        .context("bind the session scope")?;
    let intents = SqliteIntentStore::open(&config.store.db).context("open the run-state file")?;
    let closed = intents.closed();
    let samples = SampleStore::open(intents.clone())
        .await
        .context("open the samples table")?;
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
    let delivery = Arc::new(EdgeDelivery::new(
        Arc::clone(&release),
        application,
        intents,
        config.session.org.clone(),
    ));

    let (stopping, stopped) = tokio::sync::watch::channel(false);
    let ingress = Arc::new(
        Ingress::builder(
            expected_host_router(Some(release.release()), stopped.clone()),
            config.http.listen,
        )
        .build()
        .await
        .context("bind the ingress")?,
    );
    let addr = ingress.addr();
    let host = HostBuilder::default()
        .with_engine((*engine).clone())
        .with_plugin(Arc::new(routing))?
        .with_plugin(Arc::new(RouterDelivery::new(delivery.clone())))?
        .with_http_handler(ingress)
        .build()
        .context("build the host")?
        .start()
        .await
        .context("start the host")?;

    let started = host
        .workload_start(ingress_workload(&release, &config.http.route_host))
        .await
        .context("start the route guest")?;
    if started.workload_status.workload_state != WorkloadState::Running {
        let status = started.workload_status;
        let _ = stopping.send(true);
        host.stop().await?;
        anyhow::bail!("the route guest did not start: {status:?}");
    }
    let forward = match &config.forward {
        Some(forward) => match forward::start(forward, samples.clone(), stopped.clone()) {
            Ok(forward) => Some(forward),
            Err(error) => {
                let _ = stopping.send(true);
                host.stop().await?;
                return Err(error.context("start the forward"));
            }
        },
        None => None,
    };
    let device = match &config.device {
        Some(device) => match device::start(device, &release, delivery, samples.clone(), stopped) {
            Ok(device) => Some(device),
            Err(error) => {
                let _ = stopping.send(true);
                if let Some(forward) = forward {
                    forward.join().await;
                }
                host.stop().await?;
                return Err(error.context("start the device loop"));
            }
        },
        None => None,
    };
    tracing::info!(
        bundle_digest = release.bundle_digest(),
        %addr,
        device = config.device.is_some(),
        forward = config.forward.is_some(),
        "wamn-edge serves its release"
    );
    Ok(EdgeHost {
        host,
        addr,
        stopping,
        device,
        forward,
        samples,
        closed,
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

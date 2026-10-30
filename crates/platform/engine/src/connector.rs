//! Isolated connector stores and the application reader bridge.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;

use anyhow::Context as _;
use tokio::sync::{Mutex, RwLock};
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::workload::{ResolvedWorkload, WorkloadItem};
use wash_runtime::host::HostRef;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::host::egress_policy::EgressAddressPolicy;
use wash_runtime::host::http::{HostHandler, NullServer};
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin::{HostPlugin, PluginBindings, WitInterfaces};
use wash_runtime::sockets::policy::{EgressMode, SocketPolicy};
use wash_runtime::types::{Component, LocalResources, Workload};
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::TypedFunc;
use wash_runtime::wit::{WitInterface, WitWorld};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: "../runtime/wit/deps/wamn-connector",
        world: "connector",
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

pub use bindings::exports::wamn::connector::reader::ConnectorError;

pub const READER: &str = "wamn:connector/reader@0.1.0";
const BOUND: &str = "wamn:connector/bound@0.1.0";
type Read = TypedFunc<(String, Vec<u8>), (Result<Vec<u8>, ConnectorError>,)>;

/// A host-resolved binding. Credentials never enter an application store.
pub struct ConnectorBinding {
    authority: hyper::http::uri::Authority,
    credential: Option<String>,
}

impl std::fmt::Debug for ConnectorBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectorBinding")
            .field("authority", &self.authority)
            .finish_non_exhaustive()
    }
}

impl ConnectorBinding {
    /// Bind one exact authority. Wildcards and metadata destinations refuse.
    pub fn new(authority: &str, credential: Option<String>) -> anyhow::Result<Self> {
        let authority: hyper::http::uri::Authority =
            authority.parse().context("parse connector authority")?;
        anyhow::ensure!(
            authority.port_u16().is_some_and(|port| port != 0),
            "connector authority needs a nonzero port"
        );
        let host = authority
            .host()
            .trim_matches(['[', ']'])
            .trim_end_matches('.')
            .to_ascii_lowercase();
        anyhow::ensure!(
            !host.is_empty() && !host.contains(['*', '%']) && !authority.as_str().contains('@'),
            "connector authority needs an exact host"
        );
        anyhow::ensure!(
            !matches!(
                host.as_str(),
                "metadata"
                    | "metadata.google.internal"
                    | "metadata.goog"
                    | "instance-data"
                    | "instance-data.ec2.internal"
                    | "metadata.azure.internal"
                    | "host.wasmcloud.internal"
            ),
            "connector metadata or host sentinel authority is denied"
        );
        if let Ok(ip) = host.parse::<IpAddr>() {
            anyhow::ensure!(
                EgressAddressPolicy::default().permits(ip),
                "connector authority address is denied"
            );
        }
        Ok(Self {
            authority,
            credential,
        })
    }
}

// A DNS name grants public addresses only. Private ranges need a literal grant.
fn connector_address_policy(authority: &hyper::http::uri::Authority) -> EgressAddressPolicy {
    let ip = authority
        .host()
        .trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .ok()
        .map(|ip| ip.to_canonical());
    let allow_private = match ip {
        Some(IpAddr::V4(ip)) => {
            ip.is_private() || (ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1]))
        }
        Some(IpAddr::V6(ip)) => ip.segments()[0] & 0xfe00 == 0xfc00,
        None => false,
    };
    EgressAddressPolicy {
        deny_special: true,
        allow_private,
    }
}

#[async_trait::async_trait]
impl HostPlugin for ConnectorBinding {
    fn id(&self) -> &'static str {
        "wamn-connector-bound"
    }
    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from(BOUND)]),
            exports: HashSet::new(),
        }
    }
    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        _: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let authority = self.authority.to_string();
        let credential = self.credential.clone();
        let mut bound = item.linker().instance(BOUND)?;
        bound.func_wrap("authority", move |_, ()| Ok((authority.clone(),)))?;
        bound.func_wrap("credential", move |_, ()| Ok((credential.clone(),)))?;
        Ok(())
    }
}

struct ConnectorStore {
    store: Store<SharedCtx>,
    get: Read,
    // Store host references are weak. Keep the workload and handler alive.
    _workload: ResolvedWorkload,
    _egress: Arc<dyn HostHandler>,
}

/// One connector instance and one store, shared by its application calls.
pub struct Connector {
    inner: Mutex<ConnectorStore>,
    digest: String,
}

impl std::fmt::Debug for Connector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connector")
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

impl Connector {
    /// Compile and link a connector before any running store stops.
    pub async fn prepare(
        bytes: Vec<u8>,
        binding: ConnectorBinding,
    ) -> anyhow::Result<PreparedConnector> {
        let (unresolved, plugins, egress, digest) = tokio::task::spawn_blocking(move || {
            // wash-runtime sets socket policy on engines. Application Count mode
            // cannot replace the policy of this connector engine.
            let engine = crate::engine::build_engine_with_socket_policy(
                &[],
                SocketPolicy {
                    egress_mode: EgressMode::Enforce,
                    egress_addrs: connector_address_policy(&binding.authority),
                    ..SocketPolicy::default()
                },
            )?;
            let admitted = crate::component_admission::validate_connector_admission(
                &engine,
                &bytes,
                "connector",
            )?;
            let digest = admitted.digest().to_owned();
            let _compiled = admitted.component;
            let resources = LocalResources {
                allowed_hosts: Arc::from([AllowedHost::Authority(binding.authority.clone())]),
                ..LocalResources::default()
            };
            let binding: Arc<dyn HostPlugin> = Arc::new(binding);
            let plugins = HashMap::from([(binding.id(), binding)]);
            let egress: Arc<dyn HostHandler> = Arc::new(NullServer::default());
            let unresolved = engine.initialize_workload(
                &digest,
                Workload {
                    namespace: "wamn-connector".to_owned(),
                    name: digest.clone(),
                    components: vec![Component {
                        name: "connector".to_owned(),
                        digest: Some(digest.clone()),
                        bytes: bytes.into(),
                        local_resources: resources,
                        ..Component::default()
                    }],
                    host_interfaces: vec![WitInterface::from(BOUND)],
                    annotations: HashMap::new(),
                    service: None,
                    volumes: Vec::new(),
                },
            )?;
            anyhow::Ok((unresolved, plugins, egress, digest))
        })
        .await
        .context("join connector compilation")??;
        let workload = unresolved
            .resolve(
                Some(&plugins),
                &PluginBindings::new(),
                &HostRef::from_handler(&egress),
                &Meters::new(MeterKind::Off),
            )
            .await?;
        Ok(PreparedConnector {
            workload,
            egress,
            digest,
        })
    }

    /// Admit, compile and start a connector with enforced egress.
    pub async fn load(bytes: &[u8], binding: ConnectorBinding) -> anyhow::Result<Self> {
        Self::prepare(bytes.to_vec(), binding).await?.start().await
    }

    /// The identity of this store, for lifecycle observations.
    #[cfg(feature = "test-util")]
    pub async fn store_id(&self) -> String {
        self.inner
            .lock()
            .await
            .store
            .data()
            .active_ctx
            .store_id
            .to_string()
    }

    /// Call the reader without transferring any store-owned handles.
    pub async fn get(
        &self,
        connection: String,
        request: Vec<u8>,
    ) -> anyhow::Result<Result<Vec<u8>, ConnectorError>> {
        let mut inner = self.inner.lock().await;
        inner.store.set_epoch_deadline(1);
        let get = inner.get;
        match tokio::time::timeout(
            crate::engine::MAX_HOST_CALL_DURATION,
            get.call_async(&mut inner.store, (connection, request)),
        )
        .await
        {
            Ok(reply) => Ok(reply?.0),
            Err(_) => Ok(Err(ConnectorError::Timeout)),
        }
    }
}

/// A compiled workload with prepared links and no running connector store.
pub struct PreparedConnector {
    workload: ResolvedWorkload,
    egress: Arc<dyn HostHandler>,
    digest: String,
}

impl std::fmt::Debug for PreparedConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedConnector")
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

impl PreparedConnector {
    /// Start from prepared code. This step performs no compilation or registry I/O.
    pub async fn start(self) -> anyhow::Result<Connector> {
        let Self {
            workload,
            egress,
            digest,
        } = self;
        let id = workload
            .components()
            .read()
            .await
            .keys()
            .next()
            .context("connector component missing")?
            .to_string();
        let mut store = workload.new_store(&id).await?;
        // Epochs yield to the deadline future instead of imposing a ten millisecond call limit.
        store.epoch_deadline_async_yield_and_update(1);
        store.set_epoch_deadline(1);
        let pre = workload.instantiate_pre(&id).await?;
        let instance = tokio::time::timeout(
            crate::engine::MAX_HOST_CALL_DURATION,
            pre.instantiate_async(&mut store),
        )
        .await
        .context("connector startup timed out")??;
        let reader = instance
            .get_export_index(&mut store, None, READER)
            .context("connector reader missing")?;
        let get = instance
            .get_export_index(&mut store, Some(&reader), "get")
            .context("connector get missing")?;
        let get = instance.get_typed_func(&mut store, get)?;
        Ok(Connector {
            inner: Mutex::new(ConnectorStore {
                store,
                get,
                _workload: workload,
                _egress: egress,
            }),
            digest,
        })
    }
}

/// Routes application binding names to their isolated connector stores.
#[derive(Debug)]
pub struct ConnectorReader {
    connections: RwLock<HashMap<String, Arc<ConnectorSlot>>>,
}

#[derive(Debug)]
struct ConnectorSlot {
    current: RwLock<Option<Arc<Connector>>>,
}

impl ConnectorReader {
    /// Link the connectors resolved for one application.
    pub fn new(connections: HashMap<String, Arc<Connector>>) -> Self {
        Self {
            connections: RwLock::new(
                connections
                    .into_iter()
                    .map(|(name, connector)| {
                        (
                            name,
                            Arc::new(ConnectorSlot {
                                current: RwLock::new(Some(connector)),
                            }),
                        )
                    })
                    .collect(),
            ),
        }
    }

    /// Drain only this binding, then start its prepared replacement.
    /// The lifecycle owner serializes updates for this reader.
    pub async fn replace(
        &self,
        connection: &str,
        prepared: PreparedConnector,
    ) -> anyhow::Result<()> {
        let slot = self
            .connections
            .write()
            .await
            .entry(connection.to_owned())
            .or_insert_with(|| {
                Arc::new(ConnectorSlot {
                    current: RwLock::new(None),
                })
            })
            .clone();
        // Taking the writer drains admitted calls. Release it during startup.
        *slot.current.write().await = None;
        tokio::task::yield_now().await;
        let connector = prepared.start().await?;
        *slot.current.write().await = Some(Arc::new(connector));
        Ok(())
    }

    /// Observe one store without exposing its handles.
    #[cfg(feature = "test-util")]
    pub async fn store_id(&self, connection: &str) -> Option<String> {
        let slot = self.connections.read().await.get(connection).cloned()?;
        let current = slot.current.read().await;
        match current.as_ref() {
            Some(connector) => Some(connector.store_id().await),
            None => None,
        }
    }

    /// Remove bindings that the authority no longer enables.
    pub async fn retain(&self, names: &HashSet<String>) {
        self.connections
            .write()
            .await
            .retain(|name, _| names.contains(name));
    }

    /// Link this application's reader into its existing workload.
    pub fn add_to_linker(
        self: &Arc<Self>,
        item: &mut WorkloadItem<'_>,
        names: HashSet<String>,
    ) -> anyhow::Result<()> {
        let reader = Arc::clone(self);
        let names = Arc::new(names);
        item.linker().instance(READER)?.func_wrap_async(
            "get",
            move |_, (connection, request): (String, Vec<u8>)| {
                let reader = Arc::clone(&reader);
                let names = Arc::clone(&names);
                Box::new(async move {
                    let result = reader
                        .get_declared(&names, connection, request)
                        .await
                        .map_err(|error| {
                            wash_runtime::wasmtime::Error::msg(format!("{error:#}"))
                        })?;
                    Ok((result,))
                })
            },
        )?;
        Ok(())
    }

    /// Refuse a name outside the calling component's declarations.
    pub async fn get_declared(
        &self,
        names: &HashSet<String>,
        connection: String,
        request: Vec<u8>,
    ) -> anyhow::Result<Result<Vec<u8>, ConnectorError>> {
        anyhow::ensure!(
            names.contains(&connection),
            "connection undeclared: {connection}"
        );
        self.get(connection, request).await
    }

    /// Forward bytes to the named binding, or return unbound.
    pub async fn get(
        &self,
        connection: String,
        request: Vec<u8>,
    ) -> anyhow::Result<Result<Vec<u8>, ConnectorError>> {
        let slot = self.connections.read().await.get(&connection).cloned();
        let Some(slot) = slot else {
            return Ok(Err(ConnectorError::Unbound));
        };
        let current = slot.current.read().await;
        match current.as_ref() {
            Some(connector) => connector.get(connection, request).await,
            None => Ok(Err(ConnectorError::Unavailable)),
        }
    }
}

#[cfg(any(test, feature = "test-util"))]
#[async_trait::async_trait]
impl HostPlugin for ConnectorReader {
    fn id(&self) -> &'static str {
        "wamn-connector-reader"
    }
    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from(READER)]),
            exports: HashSet::new(),
        }
    }
    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        _: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        item.linker().instance(READER)?.func_wrap_async(
            "get",
            |ctx, (connection, request): (String, Vec<u8>)| {
                let reader = ctx
                    .data()
                    .active_ctx
                    .get_plugin::<Self>("wamn-connector-reader");
                Box::new(async move {
                    let result = reader.get(connection, request).await.map_err(|error| {
                        wash_runtime::wasmtime::Error::msg(format!("{error:#}"))
                    })?;
                    Ok((result,))
                })
            },
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod declaration_tests {
    use super::*;
    #[tokio::test]
    async fn an_undeclared_name_refuses_before_dispatch() {
        let reader = ConnectorReader::new(HashMap::new());
        let names = HashSet::from(["meter".to_owned()]);
        assert!(
            reader
                .get_declared(&names, "other".into(), Vec::new())
                .await
                .unwrap_err()
                .to_string()
                .contains("connection undeclared")
        );
        assert!(matches!(
            reader
                .get_declared(&names, "meter".into(), Vec::new())
                .await
                .unwrap(),
            Err(ConnectorError::Unbound)
        ));
    }
}

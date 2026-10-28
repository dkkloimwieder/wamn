//! Isolated connector stores and the application reader bridge.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;

use anyhow::Context as _;
use tokio::sync::Mutex;
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
    /// Admit and instantiate a connector under an enforced authority policy.
    pub async fn load(bytes: &[u8], binding: ConnectorBinding) -> anyhow::Result<Self> {
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
        let admitted =
            crate::component_admission::validate_connector_admission(&engine, bytes, "connector")?;
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
                    bytes: bytes.to_vec().into(),
                    local_resources: resources,
                    ..Component::default()
                }],
                host_interfaces: vec![WitInterface::from(BOUND)],
                annotations: HashMap::new(),
                service: None,
                volumes: Vec::new(),
            },
        )?;
        let workload = unresolved
            .resolve(
                Some(&plugins),
                &PluginBindings::new(),
                &HostRef::from_handler(&egress),
                &Meters::new(MeterKind::Off),
            )
            .await?;
        let id = workload
            .components()
            .read()
            .await
            .keys()
            .next()
            .context("connector component missing")?
            .to_string();
        let mut store = workload.new_store(&id).await?;
        store.set_epoch_deadline(1);
        let instance = workload
            .instantiate_pre(&id)
            .await?
            .instantiate_async(&mut store)
            .await?;
        let reader = instance
            .get_export_index(&mut store, None, READER)
            .context("connector reader missing")?;
        let get = instance
            .get_export_index(&mut store, Some(&reader), "get")
            .context("connector get missing")?;
        let get = instance.get_typed_func(&mut store, get)?;
        Ok(Self {
            inner: Mutex::new(ConnectorStore {
                store,
                get,
                _workload: workload,
                _egress: egress,
            }),
            digest,
        })
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
        let (reply,) = get
            .call_async(&mut inner.store, (connection, request))
            .await?;
        Ok(reply)
    }
}

/// Routes application binding names to their isolated connector stores.
#[derive(Debug)]
pub struct ConnectorReader {
    connections: HashMap<String, Arc<Connector>>,
}

impl ConnectorReader {
    /// Link the connectors resolved for one application.
    pub fn new(connections: HashMap<String, Arc<Connector>>) -> Self {
        Self { connections }
    }

    /// Forward bytes to the named binding, or return unbound.
    pub async fn get(
        &self,
        connection: String,
        request: Vec<u8>,
    ) -> anyhow::Result<Result<Vec<u8>, ConnectorError>> {
        match self.connections.get(&connection) {
            Some(connector) => connector.get(connection, request).await,
            None => Ok(Err(ConnectorError::Unbound)),
        }
    }
}

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

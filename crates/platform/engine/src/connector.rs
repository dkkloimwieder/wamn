//! Isolated connector stores and the application reader bridge.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::{Arc, OnceLock, PoisonError};

use anyhow::Context as _;
use tokio::sync::Mutex;
use wash_runtime::engine::Engine;
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::workload::{ResolvedWorkload, WorkloadComponent, WorkloadItem};
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
    preparation: Option<Arc<std::sync::Mutex<Option<WorkloadComponent>>>>,
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
            preparation: None,
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

struct AddressEngine {
    engine: Arc<Engine>,
    admitted: Mutex<HashMap<String, WorkloadComponent>>,
    #[cfg(any(test, feature = "test-util"))]
    images: std::sync::Mutex<HashMap<String, HashSet<(usize, usize)>>>,
}

static ADDRESS_ENGINES: OnceLock<std::sync::Mutex<HashMap<bool, Arc<AddressEngine>>>> =
    OnceLock::new();

fn address_engine(policy: EgressAddressPolicy) -> anyhow::Result<Arc<AddressEngine>> {
    let mut engines = ADDRESS_ENGINES
        .get_or_init(std::sync::Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(engine) = engines.get(&policy.allow_private) {
        return Ok(Arc::clone(engine));
    }
    let engine = Arc::new(AddressEngine {
        engine: Arc::new(crate::engine::build_engine_with_socket_policy(
            &[],
            SocketPolicy {
                egress_mode: EgressMode::Enforce,
                egress_addrs: policy,
                ..SocketPolicy::default()
            },
        )?),
        admitted: Mutex::new(HashMap::new()),
        #[cfg(any(test, feature = "test-util"))]
        images: std::sync::Mutex::new(HashMap::new()),
    });
    engines.insert(policy.allow_private, Arc::clone(&engine));
    Ok(engine)
}

/// Distinct compiled code images of this digest across the two address classes.
#[cfg(any(test, feature = "test-util"))]
pub fn compile_count(digest: &str) -> usize {
    let engines = ADDRESS_ENGINES
        .get_or_init(std::sync::Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let mut count = 0;
    for engine in engines {
        count += engine
            .images
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(digest)
            .map_or(0, HashSet::len);
    }
    count
}

fn code_workload(digest: &str, bytes: Vec<u8>, resources: LocalResources) -> Workload {
    Workload {
        namespace: "wamn-connector".to_owned(),
        name: digest.to_owned(),
        components: vec![Component {
            name: "connector".to_owned(),
            digest: Some(digest.to_owned()),
            bytes: bytes.into(),
            local_resources: resources,
            ..Component::default()
        }],
        host_interfaces: vec![WitInterface::from(BOUND)],
        annotations: HashMap::new(),
        service: None,
        volumes: Vec::new(),
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
        if let Some(preparation) = &self.preparation {
            crate::component_admission::validate_connection_code(item.component(), "connector")?;
            if let WorkloadItem::Component(component) = item {
                // Clone before linking the credential. Retain only the compiled
                // template and wash-runtime's live compile reference.
                let mut template = (**component).clone();
                template.with_plugins(HashMap::new());
                *preparation.lock().unwrap_or_else(PoisonError::into_inner) = Some(template);
            }
        }
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
        mut binding: ConnectorBinding,
    ) -> anyhow::Result<PreparedConnector> {
        let digest = crate::component_admission::component_digest(&bytes);
        let policy = connector_address_policy(&binding.authority);
        let code = tokio::task::spawn_blocking(move || address_engine(policy))
            .await
            .context("join connection engine construction")??;
        let mut admitted = code.admitted.lock().await;
        let preparation =
            (!admitted.contains_key(&digest)).then(|| Arc::new(std::sync::Mutex::new(None)));
        binding.preparation = preparation.clone();
        let engine = Arc::clone(&code.engine);
        let identity = digest.clone();
        let authority = binding.authority.clone();
        let unresolved = tokio::task::spawn_blocking(move || {
            engine.initialize_workload(
                &identity,
                code_workload(
                    &identity,
                    bytes,
                    LocalResources {
                        allowed_hosts: Arc::from([AllowedHost::Authority(authority)]),
                        ..LocalResources::default()
                    },
                ),
            )
        })
        .await
        .context("join connection linking")??;
        let binding: Arc<dyn HostPlugin> = Arc::new(binding);
        let plugins = HashMap::from([(binding.id(), binding)]);
        let egress: Arc<dyn HostHandler> = Arc::new(NullServer::default());
        let workload = unresolved
            .resolve(
                Some(&plugins),
                &PluginBindings::new(),
                &HostRef::from_handler(&egress),
                &Meters::new(MeterKind::Off),
            )
            .await?;
        let components = workload.components();
        let components = components.read().await;
        let component = components
            .values()
            .next()
            .context("connection component missing")?;
        #[cfg(any(test, feature = "test-util"))]
        {
            let image = component.component().image_range();
            code.images
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(digest.clone())
                .or_default()
                .insert((image.start as usize, image.end as usize));
        }
        if let Some(preparation) = preparation {
            let captured = preparation
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            let template = if let Some(template) = captured {
                template
            } else {
                // A reader with no bound import still needs byte admission.
                crate::component_admission::validate_connection_code(
                    component.component(),
                    "connector",
                )?;
                let mut template = component.clone();
                template.with_plugins(HashMap::new());
                template
            };
            admitted.insert(digest.clone(), template);
        }
        drop(components);
        drop(admitted);
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
    #[cfg(any(test, feature = "test-util"))]
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

/// Resolve the current immutable instance from the facts of one call.
#[async_trait::async_trait]
pub trait ConnectionDispatch: std::fmt::Debug + Send + Sync {
    async fn resolve(&self, name: &str) -> anyhow::Result<Option<Arc<Connector>>>;
}

/// Routes application binding names through the current connection facts.
#[derive(Debug)]
pub struct ConnectorReader {
    dispatch: Arc<dyn ConnectionDispatch>,
}

impl ConnectorReader {
    pub fn from_dispatch(dispatch: Arc<dyn ConnectionDispatch>) -> Self {
        Self { dispatch }
    }

    /// Observe the current store without exposing its handles.
    #[cfg(feature = "test-util")]
    pub async fn store_id(&self, connection: &str) -> anyhow::Result<Option<String>> {
        Ok(match self.dispatch.resolve(connection).await? {
            Some(instance) => Some(instance.store_id().await),
            None => None,
        })
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn new(connections: HashMap<String, Arc<Connector>>) -> Self {
        Self::from_dispatch(Arc::new(FixedReads(connections)))
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

    /// Forward bytes to the named binding, or return unbound. A binding that
    /// does not resolve answers unavailable, so the calling store stays usable.
    pub async fn get(
        &self,
        connection: String,
        request: Vec<u8>,
    ) -> anyhow::Result<Result<Vec<u8>, ConnectorError>> {
        match self.dispatch.resolve(&connection).await {
            Ok(Some(instance)) => instance.get(connection, request).await,
            Ok(None) => Ok(Err(ConnectorError::Unbound)),
            Err(error) => {
                tracing::warn!(connection, error = %format!("{error:#}"), "connection did not resolve");
                Ok(Err(ConnectorError::Unavailable))
            }
        }
    }
}

#[cfg(any(test, feature = "test-util"))]
#[derive(Debug)]
struct FixedReads(HashMap<String, Arc<Connector>>);

#[cfg(any(test, feature = "test-util"))]
#[async_trait::async_trait]
impl ConnectionDispatch for FixedReads {
    async fn resolve(&self, name: &str) -> anyhow::Result<Option<Arc<Connector>>> {
        Ok(self.0.get(name).cloned())
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
    async fn a_new_bound_credential_links_without_compiling_again() -> anyhow::Result<()> {
        let mut resolve = wit_parser::Resolve::new();
        let package = resolve.push_str(
            "read.wit",
            include_str!("../../runtime/wit/deps/wamn-connector/package.wit"),
        )?;
        let world = resolve.select_world(&[package], Some("connector"))?;
        let mut module =
            wit_component::dummy_module(&resolve, world, wit_parser::ManglingAndAbi::Standard32);
        wit_component::embed_component_metadata(
            &mut module,
            &resolve,
            world,
            wit_component::StringEncoding::UTF8,
        )?;
        let bytes = wit_component::ComponentEncoder::default()
            .module(&module)?
            .validate(true)
            .encode()?;
        let digest = crate::component_admission::component_digest(&bytes);
        let first = Connector::load(
            &bytes,
            ConnectorBinding::new("example.com:443", Some("first".into()))?,
        )
        .await?;
        assert_eq!(compile_count(&digest), 1);
        let second = Connector::load(
            &bytes,
            ConnectorBinding::new("example.com:443", Some("second".into()))?,
        )
        .await?;
        assert_eq!(compile_count(&digest), 1);
        assert_ne!(first.store_id().await, second.store_id().await);
        let code = address_engine(connector_address_policy(&"example.com:443".parse()?))?;
        assert!(
            code.admitted.lock().await[&digest]
                .pre_instantiate_ref()
                .is_err()
        );
        let mut changed = bytes;
        // A component custom section changes the digest without changing its WIT.
        changed.extend([0, 2, 1, b'v']);
        let changed_digest = crate::component_admission::component_digest(&changed);
        let third = Connector::load(
            &changed,
            ConnectorBinding::new("example.com:443", Some("third".into()))?,
        )
        .await?;
        assert_eq!(compile_count(&changed_digest), 1);
        assert_eq!(compile_count(&digest), 1);
        assert_ne!(second.store_id().await, third.store_id().await);
        Ok(())
    }

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

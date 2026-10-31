//! Isolated integration stores and named application imports.

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
use wash_runtime::wasmtime::component::{Instance, Type, Val};
use wash_runtime::wit::{WitInterface, WitWorld};

pub const HEALTH: &str = "wamn:integration/health@0.1.0";
const BOUND: &str = "wamn:integration/bound@0.1.0";

/// A host-resolved binding. Credentials never enter an application store.
pub struct IntegrationBinding {
    authority: hyper::http::uri::Authority,
    credential: Option<String>,
    parameters: String,
    preparation: Option<Arc<std::sync::Mutex<Option<WorkloadComponent>>>>,
}

impl std::fmt::Debug for IntegrationBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntegrationBinding")
            .field("authority", &self.authority)
            .finish_non_exhaustive()
    }
}

impl IntegrationBinding {
    /// Bind one exact authority. Wildcards and metadata destinations refuse.
    pub fn new(authority: &str, credential: Option<String>) -> anyhow::Result<Self> {
        let authority: hyper::http::uri::Authority =
            authority.parse().context("parse integration authority")?;
        anyhow::ensure!(
            authority.port_u16().is_some_and(|port| port != 0),
            "integration authority needs a nonzero port"
        );
        let host = authority
            .host()
            .trim_matches(['[', ']'])
            .trim_end_matches('.')
            .to_ascii_lowercase();
        anyhow::ensure!(
            !host.is_empty() && !host.contains(['*', '%']) && !authority.as_str().contains('@'),
            "integration authority needs an exact host"
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
            "integration metadata or host sentinel authority is denied"
        );
        if let Ok(ip) = host.parse::<IpAddr>() {
            anyhow::ensure!(
                EgressAddressPolicy::default().permits(ip),
                "integration authority address is denied"
            );
        }
        Ok(Self {
            authority,
            credential,
            parameters: "{}".to_owned(),
            preparation: None,
        })
    }

    /// Use validated generation parameters and expose only custom credentials.
    pub fn with_parameters(mut self, mut parameters: serde_json::Value, auth_type: &str) -> Self {
        if let Some(parameters) = parameters.as_object_mut() {
            parameters.remove("auth");
        }
        self.parameters = parameters.to_string();
        if auth_type != "custom" {
            self.credential = None;
        }
        self
    }
}

// A DNS name grants public addresses only. Private ranges need a literal grant.
fn integration_address_policy(authority: &hyper::http::uri::Authority) -> EgressAddressPolicy {
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
        namespace: "wamn-integration".to_owned(),
        name: digest.to_owned(),
        components: vec![Component {
            name: "integration".to_owned(),
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
impl HostPlugin for IntegrationBinding {
    fn id(&self) -> &'static str {
        "wamn-integration-bound"
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
            crate::component_admission::validate_connection_code(item.component(), "integration")?;
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
        let parameters = self.parameters.clone();
        let mut bound = item.linker().instance(BOUND)?;
        bound.func_wrap("authority", move |_, ()| Ok((authority.clone(),)))?;
        bound.func_wrap("parameters", move |_, ()| Ok((parameters.clone(),)))?;
        bound.func_wrap("credential", move |_, ()| Ok((credential.clone(),)))?;
        Ok(())
    }
}

struct IntegrationStore {
    store: Store<SharedCtx>,
    instance: Instance,
    // Store host references are weak. Keep the workload and handler alive.
    _workload: ResolvedWorkload,
    _egress: Arc<dyn HostHandler>,
}

/// One integration instance and one store, shared by its application calls.
pub struct Integration {
    inner: Mutex<IntegrationStore>,
    digest: String,
}

impl std::fmt::Debug for Integration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Integration")
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

impl Integration {
    /// Compile and link a integration before any running store stops.
    pub async fn prepare(
        bytes: Vec<u8>,
        mut binding: IntegrationBinding,
    ) -> anyhow::Result<PreparedIntegration> {
        let digest = crate::component_admission::component_digest(&bytes);
        let policy = integration_address_policy(&binding.authority);
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
                    "integration",
                )?;
                let mut template = component.clone();
                template.with_plugins(HashMap::new());
                template
            };
            admitted.insert(digest.clone(), template);
        }
        drop(components);
        drop(admitted);
        Ok(PreparedIntegration {
            workload,
            egress,
            digest,
        })
    }

    /// Admit, compile and start a integration with enforced egress.
    pub async fn load(bytes: &[u8], binding: IntegrationBinding) -> anyhow::Result<Self> {
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

    /// Call one typed export with values that carry no store-owned resources.
    pub async fn call(
        &self,
        interface: &str,
        function: &str,
        parameters: &[Val],
    ) -> anyhow::Result<Vec<Val>> {
        let mut inner = self.inner.lock().await;
        inner.store.set_epoch_deadline(1);
        let instance = inner.instance;
        let export = instance
            .get_export_index(&mut inner.store, None, interface)
            .context("integration interface missing")?;
        let function = instance
            .get_export_index(&mut inner.store, Some(&export), function)
            .context("integration function missing")?;
        let function = instance
            .get_func(&mut inner.store, function)
            .context("integration export is not a function")?;
        let mut results = vec![Val::Bool(false); function.ty(&inner.store).results().len()];
        tokio::time::timeout(crate::engine::MAX_HOST_CALL_DURATION, async {
            function
                .call_async(&mut inner.store, parameters, &mut results)
                .await?;
            Ok::<_, anyhow::Error>(results)
        })
        .await
        .context("integration call timed out")?
    }

    /// Test the remote service through the version's required health export.
    pub async fn health(&self) -> anyhow::Result<Result<(), String>> {
        match self.call(HEALTH, "check", &[]).await?.as_slice() {
            [Val::Result(Ok(None))] => Ok(Ok(())),
            [Val::Result(Err(Some(error)))] => match error.as_ref() {
                Val::String(error) => Ok(Err(error.clone())),
                _ => anyhow::bail!("invalid integration health error"),
            },
            _ => anyhow::bail!("invalid integration health result"),
        }
    }
}

/// A compiled workload with prepared links and no running integration store.
pub struct PreparedIntegration {
    workload: ResolvedWorkload,
    egress: Arc<dyn HostHandler>,
    digest: String,
}

impl std::fmt::Debug for PreparedIntegration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedIntegration")
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

impl PreparedIntegration {
    /// Start from prepared code. This step performs no compilation or registry I/O.
    pub async fn start(self) -> anyhow::Result<Integration> {
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
            .context("integration component missing")?
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
        .context("integration startup timed out")??;
        Ok(Integration {
            inner: Mutex::new(IntegrationStore {
                store,
                instance,
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
    async fn resolve(&self, name: &str) -> anyhow::Result<Option<Arc<Integration>>>;
}

/// Routes named imports through the current connection facts.
#[derive(Debug)]
pub struct IntegrationImports {
    dispatch: Arc<dyn ConnectionDispatch>,
    interfaces: HashSet<String>,
}

impl IntegrationImports {
    pub fn from_dispatch(
        dispatch: Arc<dyn ConnectionDispatch>,
        interfaces: HashSet<String>,
    ) -> Self {
        Self {
            dispatch,
            interfaces,
        }
    }

    pub async fn resolve(&self, name: &str) -> anyhow::Result<Option<Arc<Integration>>> {
        self.dispatch.resolve(name).await
    }

    pub fn interfaces(&self) -> impl Iterator<Item = &str> {
        self.interfaces.iter().map(String::as_str)
    }

    /// Observe the current store without exposing its handles.
    #[cfg(feature = "test-util")]
    pub async fn store_id(&self, name: &str) -> anyhow::Result<Option<String>> {
        Ok(match self.dispatch.resolve(name).await? {
            Some(instance) => Some(instance.store_id().await),
            None => None,
        })
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn new(
        connections: HashMap<String, Arc<Integration>>,
        interfaces: HashSet<String>,
    ) -> Self {
        Self::from_dispatch(Arc::new(FixedImports(connections)), interfaces)
    }

    /// Bind every declared label before the application can instantiate.
    pub async fn add_to_linker(
        self: &Arc<Self>,
        item: &mut WorkloadItem<'_>,
        names: &HashSet<String>,
    ) -> anyhow::Result<()> {
        use wash_runtime::wasmtime::component::types::ComponentItem;
        let component = item.component().clone();
        let ty = component.component_type();
        let engine = component.engine();
        for (label, import) in ty.imports(engine) {
            let Some(interface) = import.implements else {
                continue;
            };
            if !self.interfaces.contains(interface) {
                continue;
            }
            anyhow::ensure!(names.contains(label), "connection undeclared: {label}");
            anyhow::ensure!(
                self.dispatch.resolve(label).await?.is_some(),
                "unbound: {label}"
            );
            let ComponentItem::ComponentInstance(import) = import.ty else {
                anyhow::bail!("connection import is not an interface: {label}")
            };
            let mut instance = item.linker().instance(label)?;
            for (function, export) in import.exports(engine) {
                match export.ty {
                    ComponentItem::ComponentFunc(function_type) => {
                        let result_types = function_type.results().collect::<Vec<_>>();
                        let unavailable = matches!(result_types.as_slice(), [Type::Result(result)]
                            if matches!(result.err(), Some(Type::Variant(error))
                                if error.cases().any(|case| case.name == "unavailable" && case.ty.is_none())));
                        let dispatch = Arc::clone(&self.dispatch);
                        let label = label.to_owned();
                        let interface = interface.to_owned();
                        let name = function.to_owned();
                        instance.func_new_async(function, move |_, _, parameters, results| {
                            let dispatch = Arc::clone(&dispatch);
                            let label = label.clone();
                            let interface = interface.clone();
                            let name = name.clone();
                            Box::new(async move {
                                let instance =
                                    dispatch.resolve(&label).await.and_then(|instance| {
                                        instance.with_context(|| format!("unbound: {label}"))
                                    });
                                let instance = match instance {
                                    Ok(instance) => instance,
                                    Err(error) => {
                                        if unavailable {
                                            tracing::warn!(
                                                connection = label,
                                                interface,
                                                "integration did not resolve"
                                            );
                                            results[0] = Val::Result(Err(Some(Box::new(
                                                Val::Variant("unavailable".to_owned(), None),
                                            ))));
                                            return Ok(());
                                        }
                                        return Err(wash_runtime::wasmtime::Error::msg(format!(
                                            "{error:#}"
                                        )));
                                    }
                                };
                                let values = instance
                                    .call(&interface, &name, parameters)
                                    .await
                                    .map_err(|error| {
                                        wash_runtime::wasmtime::Error::msg(format!("{error:#}"))
                                    })?;
                                if values.len() != results.len() {
                                    return Err(wash_runtime::wasmtime::Error::msg(
                                        "integration result count mismatch",
                                    ));
                                }
                                results.clone_from_slice(&values);
                                Ok(())
                            })
                        })?;
                    }
                    ComponentItem::Type(_) => {}
                    _ => anyhow::bail!(
                        "integration interface contains a store-owned resource: {interface}"
                    ),
                }
            }
        }
        Ok(())
    }
}

#[cfg(any(test, feature = "test-util"))]
#[derive(Debug)]
struct FixedImports(HashMap<String, Arc<Integration>>);

#[cfg(any(test, feature = "test-util"))]
#[async_trait::async_trait]
impl ConnectionDispatch for FixedImports {
    async fn resolve(&self, name: &str) -> anyhow::Result<Option<Arc<Integration>>> {
        Ok(self.0.get(name).cloned())
    }
}

#[cfg(any(test, feature = "test-util"))]
#[async_trait::async_trait]
impl HostPlugin for IntegrationImports {
    fn id(&self) -> &'static str {
        "wamn-integration-imports"
    }
    fn world(&self) -> WitWorld {
        WitWorld {
            imports: self.interfaces().map(WitInterface::from).collect(),
            exports: HashSet::new(),
        }
    }
    fn supports_named_instances(&self) -> bool {
        true
    }
    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        _: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let imports = Arc::new(Self::from_dispatch(
            Arc::clone(&self.dispatch),
            self.interfaces.clone(),
        ));
        let ty = item.component().component_type();
        let names = ty
            .imports(item.component().engine())
            .filter(|(_, import)| {
                import
                    .implements
                    .is_some_and(|interface| self.interfaces.contains(interface))
            })
            .map(|(name, _)| name.to_owned())
            .collect();
        imports.add_to_linker(item, &names).await
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod declaration_tests {
    #[test]
    fn bound_values_remove_auth_and_expose_only_custom_credentials() {
        for auth_type in ["none", "http_basic", "custom"] {
            let binding = super::IntegrationBinding::new("example.com:443", Some("private-secret".into())).unwrap()
                .with_parameters(serde_json::json!({"authority":"example.com:443", "sample-count":3, "auth":{"name":"private-placement"}}), auth_type);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&binding.parameters).unwrap(),
                serde_json::json!({"authority":"example.com:443", "sample-count":3})
            );
            assert_eq!(
                binding.credential.as_deref(),
                (auth_type == "custom").then_some("private-secret")
            );
            assert!(!format!("{binding:?}").contains("private-secret"));
        }
    }

    use super::*;
    #[tokio::test]
    async fn a_new_bound_credential_links_without_compiling_again() -> anyhow::Result<()> {
        let mut resolve = wit_parser::Resolve::new();
        let package = resolve.push_str(
            "read.wit",
            &format!(
                "{} world fixture {{ import bound; export health; }}",
                include_str!("../../runtime/wit/deps/wamn-integration/package.wit")
            ),
        )?;
        let world = resolve.select_world(&[package], Some("fixture"))?;
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
        let first = Integration::load(
            &bytes,
            IntegrationBinding::new("example.com:443", Some("first".into()))?,
        )
        .await?;
        assert_eq!(compile_count(&digest), 1);
        let second = Integration::load(
            &bytes,
            IntegrationBinding::new("example.com:443", Some("second".into()))?,
        )
        .await?;
        assert_eq!(compile_count(&digest), 1);
        assert_ne!(first.store_id().await, second.store_id().await);
        let code = address_engine(integration_address_policy(&"example.com:443".parse()?))?;
        assert!(
            code.admitted.lock().await[&digest]
                .pre_instantiate_ref()
                .is_err()
        );
        let mut changed = bytes;
        // A component custom section changes the digest without changing its WIT.
        changed.extend([0, 2, 1, b'v']);
        let changed_digest = crate::component_admission::component_digest(&changed);
        let third = Integration::load(
            &changed,
            IntegrationBinding::new("example.com:443", Some("third".into()))?,
        )
        .await?;
        assert_eq!(compile_count(&changed_digest), 1);
        assert_eq!(compile_count(&digest), 1);
        assert_ne!(second.store_id().await, third.store_id().await);
        Ok(())
    }
}

//! Transport-free runtime for one warm `wamn:node` component.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::Mutex;
use wash_runtime::engine::Engine;
use wash_runtime::engine::ctx::{ActiveCtx, Ctx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::host::http::{
    DefaultOutgoingHandler, HostHandler, OutgoingHandler as _, check_allowed_hosts,
};
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component, InstancePre, Linker};
use wash_runtime::wit::{WitInterface, WitWorld};
use wasmtime_wasi_http::p2::HttpResult;
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::p2::types::{HostFutureIncomingResponse, OutgoingRequestConfig};

use wamn_component_policy::{PolicyProfile, analyze};
use wamn_node_invoke::{
    NodeInvokeRequest, NodeInvokeResponse, SIGNING_KEY_CREDENTIAL, SIGNING_KEY_CREDENTIAL_PREVIOUS,
    WireEmission, WireErrorDetail, WireNodeError, WirePayload, WireRateLimit,
};

/// Default identity for a single-node runtime.
pub const DEFAULT_NODE_ID: &str = "serve-node";

/// A provider-side credential lookup outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialLookupError {
    NotFound,
    Unavailable,
}

/// Project-scoped credential source used by a node runtime.
pub trait CredentialProvider: Send + Sync {
    fn get(&self, project: &str, name: &str) -> Result<String, CredentialLookupError>;
}

/// Credential provider used by builder conformance: every granted read is unavailable.
#[derive(Debug, Default)]
pub struct DenyAllCredentials;

impl CredentialProvider for DenyAllCredentials {
    fn get(&self, _project: &str, _name: &str) -> Result<String, CredentialLookupError> {
        Err(CredentialLookupError::Unavailable)
    }
}

/// Runtime egress decision over an outbound HTTP request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressRequest {
    pub method: String,
    pub uri: String,
}

/// Runtime egress decision over a transport-neutral request description.
pub trait EgressPolicy: Send + Sync {
    fn allows(&self, request: &EgressRequest) -> bool;
}

/// Egress policy used by builder conformance: no outbound request is admitted.
#[derive(Debug, Default)]
pub struct DenyAllEgress;

impl EgressPolicy for DenyAllEgress {
    fn allows(&self, _request: &EgressRequest) -> bool {
        false
    }
}

/// Production-compatible egress policy backed by wash-runtime allowed-host rules.
pub struct AllowedHostsEgress {
    hosts: Arc<[AllowedHost]>,
}

impl AllowedHostsEgress {
    /// Parse allowed-host patterns without exposing the transport
    /// implementation's concrete rule type.
    pub fn parse(hosts: impl IntoIterator<Item = impl AsRef<str>>) -> anyhow::Result<Self> {
        let hosts = hosts
            .into_iter()
            .map(|host| host.as_ref().parse::<AllowedHost>())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            hosts: hosts.into(),
        })
    }
}

impl EgressPolicy for AllowedHostsEgress {
    fn allows(&self, request: &EgressRequest) -> bool {
        let Ok(request) = hyper::Request::builder()
            .method(request.method.as_str())
            .uri(request.uri.as_str())
            .body(())
        else {
            return false;
        };
        check_allowed_hosts(&request, &self.hosts).is_ok()
    }
}

/// Inputs required to instantiate one node component.
pub struct NodeRuntimeConfig {
    pub component_id: String,
    pub project: String,
    pub credentials: Arc<dyn CredentialProvider>,
    pub egress: Arc<dyn EgressPolicy>,
}

impl NodeRuntimeConfig {
    /// Builder/test posture with no credentials and deny-all egress.
    pub fn deny_all(component_id: impl Into<String>, project: impl Into<String>) -> Self {
        Self {
            component_id: component_id.into(),
            project: project.into(),
            credentials: Arc::new(DenyAllCredentials),
            egress: Arc::new(DenyAllEgress),
        }
    }
}

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "serve-node",
        path: "../runtime/wit",
        imports: { default: async },
        exports: { default: async },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

mod credential_bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "credentials-plugin",
        path: "../runtime/wit",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::ServeNode as NodeBindings;
use bindings::exports::wamn::node::handler::{Emission, NodeError, Payload, RunContext};
use credential_bindings::wamn::node::credentials::{self, CredentialError};

const NODE_CREDENTIALS_ID: &str = "wamn-node-runtime-credentials";

struct NodeCredentials {
    project: String,
    provider: Arc<dyn CredentialProvider>,
    grants: std::sync::RwLock<Option<HashSet<String>>>,
}

impl NodeCredentials {
    fn set_grant(&self, names: impl IntoIterator<Item = String>) {
        *self
            .grants
            .write()
            .expect("node credential grant lock poisoned") = Some(names.into_iter().collect());
    }

    fn clear_grant(&self) {
        *self
            .grants
            .write()
            .expect("node credential grant lock poisoned") = None;
    }

    fn has_active_grant(&self) -> bool {
        self.grants
            .read()
            .expect("node credential grant lock poisoned")
            .is_some()
    }

    fn get(&self, name: &str) -> Result<String, CredentialError> {
        if !self
            .grants
            .read()
            .expect("node credential grant lock poisoned")
            .as_ref()
            .is_some_and(|grant| grant.contains(name))
        {
            return Err(CredentialError::NotGranted);
        }
        self.provider
            .get(&self.project, name)
            .map_err(|error| match error {
                CredentialLookupError::NotFound => CredentialError::NotFound,
                CredentialLookupError::Unavailable => CredentialError::Unavailable,
            })
    }
}

#[async_trait::async_trait]
impl HostPlugin for NodeCredentials {
    fn id(&self) -> &'static str {
        NODE_CREDENTIALS_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:node/credentials@0.1.0")]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        _item: &mut WorkloadItem<'a>,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

fn node_credentials(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<NodeCredentials>> {
    ctx.try_get_plugin::<NodeCredentials>(NODE_CREDENTIALS_ID)
}

impl credentials::Host for ActiveCtx<'_> {
    async fn get(
        &mut self,
        handle: String,
    ) -> wash_runtime::wasmtime::Result<Result<String, CredentialError>> {
        Ok(node_credentials(self)?.get(&handle))
    }
}

struct RuntimeEgress {
    inner: DefaultOutgoingHandler,
    policy: Arc<dyn EgressPolicy>,
}

#[async_trait::async_trait]
impl HostHandler for RuntimeEgress {
    async fn start(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn port(&self) -> u16 {
        0
    }

    async fn on_workload_resolved(
        &self,
        _resolved: &wash_runtime::engine::workload::ResolvedWorkload,
        _component_id: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_workload_unbind(&self, _workload_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn outgoing_request(
        &self,
        workload_id: &str,
        request: hyper::Request<HyperOutgoingBody>,
        config: OutgoingRequestConfig,
        _allowed_hosts: &[AllowedHost],
    ) -> HttpResult<HostFutureIncomingResponse> {
        let description = EgressRequest {
            method: request.method().to_string(),
            uri: request.uri().to_string(),
        };
        if !self.policy.allows(&description) {
            return Ok(HostFutureIncomingResponse::ready(Ok(Err(
                ErrorCode::HttpRequestDenied,
            ))));
        }
        self.inner.send_request(workload_id, request, config)
    }
}

struct NodeInstance {
    store: Store<SharedCtx>,
    bindings: NodeBindings,
    config_cache: ConfigCache,
}

impl NodeInstance {
    async fn run(
        &mut self,
        context: &RunContext,
        input: &Payload,
    ) -> wash_runtime::wasmtime::Result<Result<Emission, NodeError>> {
        self.bindings
            .wamn_node_handler()
            .call_run(&mut self.store, context, input)
            .await
    }
}

/// One warm, sequentially-invoked custom node component.
pub struct NodeRuntime {
    instance: Mutex<NodeInstance>,
    credentials: Arc<NodeCredentials>,
    grant_installs: AtomicU64,
}

impl NodeRuntime {
    /// Compile, policy-screen, link, and warm-instantiate a custom node.
    pub async fn instantiate(
        engine: &Engine,
        wasm: &[u8],
        config: NodeRuntimeConfig,
    ) -> anyhow::Result<Self> {
        let imports = wamn_runtime::component_imports(engine, wasm, &config.component_id)?;
        analyze(&imports, PolicyProfile::Tenant, &config.component_id)
            .map_err(|error| anyhow::anyhow!("node runtime refuses this node: {error}"))?;

        let raw = engine.inner();
        let component =
            Component::new(raw, wasm).map_err(|error| anyhow::anyhow!("compile node: {error}"))?;
        let mut linker: Linker<SharedCtx> = Linker::new(raw);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
        credentials::add_to_linker::<_, SharedCtx>(&mut linker, extract_active_ctx)?;
        wamn_runtime::plugins::wamn_node::add_to_linker(&mut linker)?;
        let pre: InstancePre<SharedCtx> = linker.instantiate_pre(&component)?;

        let credentials = Arc::new(NodeCredentials {
            project: config.project,
            provider: config.credentials,
            grants: std::sync::RwLock::new(None),
        });
        let mut plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::new();
        plugins.insert(
            NODE_CREDENTIALS_ID,
            credentials.clone() as Arc<dyn HostPlugin + Send + Sync>,
        );
        let context = Ctx::builder(config.component_id.clone(), config.component_id)
            .with_plugins(plugins)
            .with_http_handler(Arc::new(RuntimeEgress {
                inner: DefaultOutgoingHandler,
                policy: config.egress,
            }))
            .build();
        let mut store = Store::new(raw, SharedCtx::new(context));
        store.set_epoch_deadline(u64::MAX / 2);
        let instance = pre.instantiate_async(&mut store).await?;
        let bindings = NodeBindings::new(&mut store, &instance)?;

        Ok(Self {
            instance: Mutex::new(NodeInstance {
                store,
                bindings,
                config_cache: ConfigCache::default(),
            }),
            credentials,
            grant_installs: AtomicU64::new(0),
        })
    }

    /// Invoke the warm node with one request and a grant scoped to that call.
    pub async fn invoke(&self, request: NodeInvokeRequest) -> NodeInvokeResponse {
        // Serialize BEFORE mutating the shared grant. Installing it outside
        // this critical section lets a waiting invocation overwrite or revoke
        // the grant of the invocation currently executing in the warm store.
        let mut instance = self.instance.lock().await;
        let grant = request.grant.into_iter().filter(|name| {
            name != SIGNING_KEY_CREDENTIAL && name != SIGNING_KEY_CREDENTIAL_PREVIOUS
        });
        self.credentials.set_grant(grant);
        self.grant_installs.fetch_add(1, Ordering::Relaxed);
        let _revoke = GrantGuard(&self.credentials);

        if let Err(error) = instance.config_cache.prepared(
            &request.ctx.node_id,
            request.ctx.flow_version,
            &request.ctx.config,
        ) {
            return NodeInvokeResponse::Err(WireNodeError::InvalidInput(WireErrorDetail {
                message: error.to_string(),
                code: Some("invalid-config".to_string()),
                data: None,
            }));
        }

        let context = RunContext {
            run_id: request.ctx.run_id,
            flow_id: request.ctx.flow_id,
            flow_version: request.ctx.flow_version,
            node_id: request.ctx.node_id,
            attempt: request.ctx.attempt,
            // Frozen 0.1 ABI field: retain the layout without minting authority.
            idempotency_key: String::new(),
            traceparent: request.ctx.traceparent,
            tracestate: request.ctx.tracestate,
            deadline_ms: request.ctx.deadline_ms,
            config: request.ctx.config,
            context: request.ctx.context,
        };
        let input = Payload::Inline(request.input.inline().unwrap_or("null").to_string());
        match instance.run(&context, &input).await {
            Ok(Ok(emission)) => NodeInvokeResponse::Ok(emission_to_wire(emission)),
            Ok(Err(error)) => NodeInvokeResponse::Err(error_to_wire(error)),
            Err(trap) => NodeInvokeResponse::Err(WireNodeError::Retryable(WireErrorDetail {
                message: format!("node invocation trapped: {trap}"),
                code: Some("node-trap".to_string()),
                data: None,
            })),
        }
    }

    pub async fn config_parse_count(&self) -> u64 {
        self.instance.lock().await.config_cache.parse_count()
    }

    pub fn grant_install_count(&self) -> u64 {
        self.grant_installs.load(Ordering::Relaxed)
    }

    /// Whether an invocation currently owns a credential grant.
    pub fn invocation_grant_active(&self) -> bool {
        self.credentials.has_active_grant()
    }
}

struct GrantGuard<'a>(&'a NodeCredentials);

impl Drop for GrantGuard<'_> {
    fn drop(&mut self) {
        self.0.clear_grant();
    }
}

fn payload_to_wire(payload: Payload) -> WirePayload {
    match payload {
        Payload::Inline(value) => WirePayload::Inline(value),
        Payload::Streamed(reference) => {
            WirePayload::Inline(format!("{{\"streamed\":{:?}}}", reference.handle))
        }
    }
}

fn emission_to_wire(emission: Emission) -> WireEmission {
    WireEmission {
        payload: payload_to_wire(emission.payload),
        port: emission.port,
        ctx: emission.ctx,
    }
}

fn detail_to_wire(detail: bindings::wamn::node::types::ErrorDetail) -> WireErrorDetail {
    WireErrorDetail {
        message: detail.message,
        code: detail.code,
        data: detail.data,
    }
}

fn error_to_wire(error: NodeError) -> WireNodeError {
    match error {
        NodeError::Retryable(detail) => WireNodeError::Retryable(detail_to_wire(detail)),
        NodeError::RateLimited(rate) => WireNodeError::RateLimited(WireRateLimit {
            detail: detail_to_wire(rate.detail),
            retry_after_ms: rate.retry_after_ms,
            target_host: rate.target_host,
        }),
        NodeError::Terminal(detail) => WireNodeError::Terminal(detail_to_wire(detail)),
        NodeError::InvalidInput(detail) => WireNodeError::InvalidInput(detail_to_wire(detail)),
        NodeError::Cancelled => WireNodeError::Cancelled,
    }
}

#[derive(Debug)]
struct ConfigError {
    message: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "config is not valid JSON: {}", self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConfigKey {
    node_id: String,
    flow_version: u32,
    config: String,
}

#[derive(Debug, Default)]
struct ConfigCache {
    entries: HashMap<ConfigKey, Arc<serde_json::Value>>,
    parses: u64,
}

impl ConfigCache {
    fn prepared(
        &mut self,
        node_id: &str,
        flow_version: u32,
        config: &str,
    ) -> Result<Arc<serde_json::Value>, ConfigError> {
        let key = ConfigKey {
            node_id: node_id.to_string(),
            flow_version,
            config: config.to_string(),
        };
        if let Some(value) = self.entries.get(&key) {
            return Ok(value.clone());
        }
        let value: serde_json::Value =
            serde_json::from_str(config).map_err(|error| ConfigError {
                message: error.to_string(),
            })?;
        self.parses += 1;
        let value = Arc::new(value);
        self.entries.insert(key, value.clone());
        Ok(value)
    }

    fn parse_count(&self) -> u64 {
        self.parses
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        ConfigCache, DenyAllCredentials, Emission, NodeCredentials, Payload, emission_to_wire,
    };
    use wamn_node_invoke::WirePayload;

    #[test]
    fn config_cache_parses_once_per_node_version_and_config() {
        let mut cache = ConfigCache::default();

        cache.prepared("node", 1, r#"{"mode":"a"}"#).unwrap();
        cache.prepared("node", 1, r#"{"mode":"a"}"#).unwrap();
        assert_eq!(cache.parse_count(), 1);

        cache.prepared("node", 1, r#"{"mode":"b"}"#).unwrap();
        cache.prepared("node", 2, r#"{"mode":"b"}"#).unwrap();
        cache.prepared("other", 2, r#"{"mode":"b"}"#).unwrap();
        assert_eq!(cache.parse_count(), 4);
    }

    #[test]
    fn invalid_config_is_not_cached() {
        let mut cache = ConfigCache::default();

        assert!(cache.prepared("node", 1, "{").is_err());
        assert!(cache.prepared("node", 1, "{").is_err());
        assert_eq!(cache.parse_count(), 0);
    }

    #[test]
    fn successful_emission_preserves_replacement_context_on_the_wire() {
        let wire = emission_to_wire(Emission {
            payload: Payload::Inline("null".to_string()),
            port: None,
            ctx: Some(r#"{"hold":{"id":7}}"#.to_string()),
        });

        assert_eq!(wire.payload, WirePayload::Inline("null".to_string()));
        assert_eq!(wire.ctx.as_deref(), Some(r#"{"hold":{"id":7}}"#));
    }

    #[test]
    fn credential_grant_is_active_only_until_revoked() {
        let credentials = NodeCredentials {
            project: "project".to_string(),
            provider: Arc::new(DenyAllCredentials),
            grants: std::sync::RwLock::new(None),
        };

        assert!(!credentials.has_active_grant());
        credentials.set_grant(["declared".to_string()]);
        assert!(credentials.has_active_grant());
        credentials.clear_grant();
        assert!(!credentials.has_active_grant());
    }
}

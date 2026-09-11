//! The single production driver for direct and queued wiring delivery.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::router_response::{PartialEvidence, PreparedResponse, ResponseState};
use anyhow::Context as _;
use futures_util::{StreamExt as _, stream};
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::TraceContextExt as _;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use wamn_catalog::{
    AdmittedComponent, ArtifactHash, AttachmentKind, ComponentOperationDependency,
    ComponentSqlField, ComponentSqlValueType, DefinitionHash, ServingComponent,
    ServingComponentOperation, ServingManifest, ServingWiring,
};
use wamn_control_registry::identifiers::valid_runner;
use wamn_event_wire::Causation;
use wamn_router::{
    ActiveWiring, CacheInsert, Delivery, ErrorDetail, Lookup, NodeError, NodeOutcome, Outcome,
    RateLimitDetail, Step, WiringCache, WiringCacheSnapshot,
};
use wamn_runtime::component_artifact_source::{
    ComponentArtifactFetchErrorKind, ComponentArtifactSource,
};
use wamn_runtime::engine::MAX_HOST_CALL_DURATION;
use wamn_runtime::plugins::EffectEvidence;
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::connection_http::{
    ConnectionExecutionClosure, ConnectionHttp, ConnectionInvocation, ConnectionOrigin,
};
use wamn_runtime::plugins::flow_http_routing::{AuthenticatedCaller, CredentialKind};
use wamn_runtime::plugins::wamn_blobstore::plugin::WamnBlobstore;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{
    CandidateBindingWorld, CandidateWiringResolution, PreparedStatementSet, ReleaseIdentity,
    ResolvedActiveWiring, SessionClaims, StatementField, StatementValueType, VerifiedStatement,
    VerifiedStatementSet, WamnPostgres,
};
use wamn_runtime::release_manifest::ReleaseManifestWeld;
use wamn_runtime::wiring_doorbell::WiringDoorbellListener;
use wash_runtime::engine::Engine;
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wit::WitInterface;

mod native_call;
mod native_policy;
mod native_workload;

use native_call::{NativeInvocation, invoke_native, prepare_native};
use native_policy::{NATIVE_POLICY_ID, NativePolicyResources, new_native_policy};
use native_workload::{
    NativeApplication, NativeComponent, NativeWorkloadSpec, load_native_application,
};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: "../router/wit",
        world: "node",
        exports: { default: async },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::node::types as node_types;

/// Shared CLI/environment key for the only wiring cache in a serving process.
pub const WIRING_CACHE_CAPACITY_ENV: &str = "WAMN_WIRING_CACHE_CAPACITY";

/// Default entries in the process-local wiring cache.
///
/// Entries are parsed documents plus immutable catalog pointers (roughly KiB),
/// while the production working set is hundreds of active wirings per
/// environment. 1,024 therefore costs single-digit MiB and cheaply avoids hot
/// path re-parsing; the hit/eviction metrics make the choice evidence-tunable.
pub const DEFAULT_WIRING_CACHE_CAPACITY: usize = 1_024;

/// Keep at most two verified artifact fetches in flight per release load.
/// Native workload loading owns compilation after these bounded fetches finish.
const COMPONENT_FETCH_CONCURRENCY: usize = 2;

/// A non-zero wiring cache bound, parsed once at process construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WiringCacheCapacity(NonZeroUsize);

impl WiringCacheCapacity {
    pub fn get(self) -> NonZeroUsize {
        self.0
    }
}

impl Default for WiringCacheCapacity {
    fn default() -> Self {
        Self(NonZeroUsize::new(DEFAULT_WIRING_CACHE_CAPACITY).expect("default is non-zero"))
    }
}

impl fmt::Display for WiringCacheCapacity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for WiringCacheCapacity {
    type Err = InvalidWiringCacheCapacity;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parsed = value
            .parse::<usize>()
            .ok()
            .and_then(NonZeroUsize::new)
            .ok_or(InvalidWiringCacheCapacity)?;
        Ok(Self(parsed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidWiringCacheCapacity;

impl fmt::Display for InvalidWiringCacheCapacity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("wiring cache capacity must be a non-zero integer")
    }
}

impl std::error::Error for InvalidWiringCacheCapacity {}

/// Process-owned construction facts shared by the host and executor leaves.
#[derive(Debug, Clone)]
pub struct RouterDriverConfig {
    pub owner_prefix: String,
    pub project: String,
    pub schema: Option<String>,
    pub cache_capacity: WiringCacheCapacity,
}

/// Which resolution authority one delivery carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WiringResolution {
    /// A trusted attachment/registration resolved the current pointer. The DB
    /// rechecks that exact version is still active.
    Active,
    /// The released attachment or queue admission already froze this immutable
    /// version. A miss resolves that exact release wiring; pointer flips never
    /// reinterpret it.
    Frozen,
}

/// Why an originating caller cannot invoke a registered operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationRefusalKind {
    PermissionDenied,
    FreshCredentialRequired,
}

/// Exact operation authority missing from the originating caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperationRefusal {
    kind: OperationRefusalKind,
    operation: Box<str>,
}

impl OperationRefusal {
    pub(crate) fn new(kind: OperationRefusalKind, operation: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            operation: operation.into(),
        }
    }

    pub(crate) fn kind(&self) -> OperationRefusalKind {
        self.kind
    }

    pub(crate) fn operation(&self) -> &str {
        &self.operation
    }
}

impl fmt::Display for OperationRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.kind {
            OperationRefusalKind::PermissionDenied => "permission denied",
            OperationRefusalKind::FreshCredentialRequired => "fresh-credential-required",
        };
        write!(formatter, "{reason} for operation {}", self.operation)
    }
}

impl std::error::Error for OperationRefusal {}

pub(crate) fn authorize_registered_operation(
    caller: Option<&AuthenticatedCaller>,
    operation: Option<&str>,
    fresh_only: bool,
) -> Result<(), OperationRefusal> {
    let Some(operation) = operation else {
        return Ok(());
    };
    let caller = caller
        .filter(|caller| caller.permits(operation))
        .ok_or_else(|| OperationRefusal::new(OperationRefusalKind::PermissionDenied, operation))?;
    if fresh_only && caller.credential_kind() == CredentialKind::Session {
        return Err(OperationRefusal::new(
            OperationRefusalKind::FreshCredentialRequired,
            operation,
        ));
    }
    Ok(())
}

/// Stable host classification for a candidate fact that cannot be retried
/// into correctness. Availability failures deliberately use their original
/// error types and remain queue-retryable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateExecutionRefusalKind {
    Identity,
    Definition,
    Binding,
    Artifact,
}

/// Typed deterministic refusal returned by candidate preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateExecutionRefusal {
    kind: CandidateExecutionRefusalKind,
    refusal: &'static str,
}

impl CandidateExecutionRefusal {
    fn new(kind: CandidateExecutionRefusalKind, refusal: &'static str) -> Self {
        Self { kind, refusal }
    }

    /// Host-only class used by the queue persistence adapter.
    pub fn kind(&self) -> CandidateExecutionRefusalKind {
        self.kind
    }

    /// Frozen refusal literal persisted in the candidate run result.
    pub fn refusal(&self) -> &'static str {
        self.refusal
    }
}

impl fmt::Display for CandidateExecutionRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.refusal)
    }
}

impl std::error::Error for CandidateExecutionRefusal {}

/// Trusted coordinates handed from an ingress admission owner to the driver.
#[derive(Debug, Clone)]
pub struct RouterDriverRequest {
    pub tenant_id: String,
    pub package_id: String,
    pub environment: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub delivery_id: String,
    pub payload: serde_json::Value,
    pub caller_attached: bool,
    pub resolution: WiringResolution,
    pub caller: Option<AuthenticatedCaller>,
    pub traceparent: Option<String>,
    pub tracestate: Option<String>,
}

struct TraceHeaders<'a> {
    traceparent: &'a str,
    tracestate: Option<&'a str>,
}

impl Extractor for TraceHeaders<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        if key.eq_ignore_ascii_case("traceparent") {
            Some(self.traceparent)
        } else if key.eq_ignore_ascii_case("tracestate") {
            self.tracestate
        } else {
            None
        }
    }

    fn keys(&self) -> Vec<&str> {
        match self.tracestate {
            Some(_) => vec!["traceparent", "tracestate"],
            None => vec!["traceparent"],
        }
    }
}

/// The pair of W3C fields the router hands one guest, written by the global
/// propagator.
///
/// The mirror image of [`TraceHeaders`]: that one reads the caller's headers in,
/// this one writes the host's own span out.
#[derive(Debug, Default, PartialEq, Eq)]
struct NodeTraceContext {
    traceparent: Option<String>,
    tracestate: Option<String>,
}

impl opentelemetry::propagation::Injector for NodeTraceContext {
    fn set(&mut self, key: &str, value: String) {
        // The W3C propagator writes `tracestate` even when the context carries
        // none, and an empty field is not a field.
        let value = (!value.is_empty()).then_some(value);
        if key.eq_ignore_ascii_case("traceparent") {
            self.traceparent = value;
        } else if key.eq_ignore_ascii_case("tracestate") {
            self.tracestate = value;
        }
    }
}

/// The context the guest — and every hop below it — must parent to: the
/// `wamn.component.invoke` span this node is running under, NOT the raw ingress
/// header that opened the delivery.
///
/// Forwarding the ingress header verbatim made every downstream service a
/// sibling of the host rather than its child, skipping the component span
/// entirely, and left the queue path (which carries no ingress header at all,
/// by the ratified host-scoped re-root) with no context to send.
///
/// Read through [`tracing_opentelemetry::OpenTelemetrySpanExt`], never
/// `opentelemetry::global::tracer`: the runtime's `initialize_observability`
/// installs the layer but no global tracer provider, so the global tracer is a
/// silent no-op.
fn node_trace_context(request: &RouterDriverRequest) -> NodeTraceContext {
    let mut carrier = NodeTraceContext::default();
    let context = tracing::Span::current().context();
    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut carrier);
    });
    if carrier.traceparent.is_none() {
        // A tracing subscriber without the OTel layer is the supported
        // no-export mode: the span has no exportable context to inject, so pass
        // the caller's own header through rather than dropping propagation.
        carrier.traceparent = request.traceparent.clone();
        carrier.tracestate = request.tracestate.clone();
    }
    carrier
}

/// The exact context one `wamn:node` guest is invoked with.
///
/// A free function, not an inline literal in `invoke_node`, because the trace
/// fields are the only part of it a caller cannot see: `invoke_node` needs a
/// live engine and real component bytes, and the guest's parentage has to be
/// provable without either.
///
/// The caller instruments this invocation with the node's
/// `wamn.component.invoke` span, so [`node_trace_context`] reads THAT span.
fn node_context(
    request: &RouterDriverRequest,
    wiring_version: u32,
    call: &wamn_router::NodeCall,
    deadline_ms: u64,
) -> anyhow::Result<node_types::NodeContext> {
    let trace = node_trace_context(request);
    Ok(node_types::NodeContext {
        wiring_id: request.wiring_id.clone(),
        wiring_version,
        node_id: call.node.clone(),
        delivery_id: request.delivery_id.clone(),
        input_port: call.input_port.clone(),
        occurrence: call.occurrence,
        traceparent: trace.traceparent,
        tracestate: trace.tracestate,
        deadline_ms: Some(deadline_ms),
        config: serde_json::to_string(&call.config).context("encode node config")?,
    })
}

fn remote_trace_context(request: &RouterDriverRequest) -> Option<opentelemetry::Context> {
    let traceparent = request.traceparent.as_deref()?;
    let headers = TraceHeaders {
        traceparent,
        tracestate: request.tracestate.as_deref(),
    };
    let context =
        opentelemetry::global::get_text_map_propagator(|propagator| propagator.extract(&headers));
    if context.span().span_context().is_valid() {
        Some(context)
    } else {
        None
    }
}

fn component_invocation_span(
    request: &RouterDriverRequest,
    project: &str,
    wiring_version: u32,
    component_digest: &str,
    call: &wamn_router::NodeCall,
    remote_parent: Option<&opentelemetry::Context>,
) -> tracing::Span {
    let span = tracing::info_span!(
        target: "wamn::router",
        "wamn.component.invoke",
        wamn.tenant = %request.tenant_id,
        wamn.project = %project,
        wamn.environment = %request.environment,
        wamn.wiring_id = %request.wiring_id,
        wamn.wiring_version = wiring_version,
        wamn.component_digest = %component_digest,
        wamn.node_id = %call.node,
        wamn.operation = %call.operation,
        wamn.caller_principal_id = tracing::field::Empty,
        wamn.caller_credential_kind = tracing::field::Empty,
        wamn.input_port = tracing::field::Empty,
    );
    if let Some(caller) = request.caller.as_ref() {
        span.record("wamn.caller_principal_id", caller.principal_id());
        span.record(
            "wamn.caller_credential_kind",
            match caller.credential_kind() {
                CredentialKind::Pat => "pat",
                CredentialKind::Session => "session",
            },
        );
    }
    if let Some(input_port) = call.input_port.as_deref() {
        span.record("wamn.input_port", input_port);
    }
    if let Some(parent) = remote_parent {
        // A tracing subscriber without the OTel layer is the supported
        // no-export mode. `set_parent` may then refuse; the span still records
        // locally and no invocation is affected.
        let _ = span.set_parent(parent.clone());
    }
    span
}

/// Exact immutable candidate selected by one durable management admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateWiringTarget {
    pub tenant_id: String,
    pub package_id: String,
    pub environment: String,
    pub effective_release_id: u32,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub wiring_hash: String,
}

/// One queued candidate input executed through the production driver.
#[derive(Debug, Clone)]
pub struct CandidateCaseRequest {
    pub target: CandidateWiringTarget,
    pub binding_world: Arc<CandidateBindingWorld>,
    pub delivery_id: String,
    pub payload: serde_json::Value,
    pub traceparent: Option<String>,
    pub tracestate: Option<String>,
}

/// One completely walked delivery, including the exact graph identity used.
#[derive(Debug, Clone)]
pub struct RouterDelivery {
    pub wiring_version: u32,
    pub graph_hash: Arc<str>,
    pub outcome: Outcome,
    pub(crate) partial: Option<PartialEvidence>,
}

/// Read-only lifecycle totals for the bounded driver store.
#[derive(Debug, Clone)]
pub struct RouterDriverSnapshot {
    pub wiring_cache: WiringCacheSnapshot,
}

/// The synchronous release closure made resident by one readiness evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreparedReleaseReadiness {
    pub(crate) synchronous_wirings: usize,
    pub(crate) component_digests: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct CatalogFacts {
    effective_release_id: u32,
    components: Arc<[AdmittedComponent]>,
    node_components: Arc<BTreeMap<String, AdmittedComponent>>,
    response: Option<Arc<PreparedResponse>>,
}

impl CatalogFacts {
    fn from_resolved(resolved: &ResolvedActiveWiring) -> anyhow::Result<Self> {
        Ok(Self {
            effective_release_id: resolved.effective_release_id,
            components: Arc::clone(&resolved.components),
            node_components: Arc::clone(&resolved.node_components),
            response: PreparedResponse::from_resolved(resolved)?.map(Arc::new),
        })
    }

    fn component(&self, node: &str) -> Option<&AdmittedComponent> {
        self.node_components.get(node)
    }
}

#[derive(Debug, Clone, Copy)]
enum ExecutionClosure<'a> {
    Released,
    Candidate {
        target: &'a CandidateWiringTarget,
        binding_world: &'a Arc<CandidateBindingWorld>,
        application: &'a Arc<NativeApplication>,
    },
}

/// One router, cache, and artifact source per serving process. Both process
/// leaves construct this exact type.
pub struct RouterDriver {
    engine: Arc<Engine>,
    postgres: Arc<WamnPostgres>,
    /// Shared by every driver in this process, independently of fresh stores.
    http_transport: Arc<HttpTransport>,
    credentials: Arc<WamnCredentials>,
    logging: Arc<WamnLogging>,
    allowed_hosts: Arc<[AllowedHost]>,
    release: Arc<ReleaseManifestWeld>,
    source: ComponentArtifactSource,
    config: RouterDriverConfig,
    cache: Arc<WiringCache<CatalogFacts>>,
    native: tokio::sync::OnceCell<Arc<NativeApplication>>,
    _doorbell: WiringDoorbellListener,
    started: Instant,
}

impl fmt::Debug for RouterDriver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RouterDriver")
            .field("config", &self.config)
            .field("cache", &self.cache.snapshot())
            .finish_non_exhaustive()
    }
}

impl RouterDriver {
    /// Bind one release to the process-owned capabilities.
    /// Every driver in the same process must receive the same HTTP transport.
    #[expect(
        clippy::too_many_arguments,
        reason = "each host-owned capability is an independent production dependency"
    )]
    pub fn new(
        engine: Arc<Engine>,
        postgres: Arc<WamnPostgres>,
        http_transport: Arc<HttpTransport>,
        credentials: Arc<WamnCredentials>,
        logging: Arc<WamnLogging>,
        allowed_hosts: Arc<[AllowedHost]>,
        release: Arc<ReleaseManifestWeld>,
        source: ComponentArtifactSource,
        config: RouterDriverConfig,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            valid_runner(&config.owner_prefix),
            "invalid router owner {:?}: 1-128 chars of [A-Za-z0-9_-] required",
            config.owner_prefix
        );
        let cache = Arc::new(WiringCache::new(config.cache_capacity.get()));
        let doorbell = WiringDoorbellListener::postgres(
            Arc::clone(&postgres),
            Some(config.project.clone()),
            Arc::clone(&cache),
        )?;
        Ok(Self {
            engine,
            postgres,
            http_transport,
            credentials,
            logging,
            allowed_hosts,
            release,
            source,
            config,
            cache,
            native: tokio::sync::OnceCell::new(),
            _doorbell: doorbell,
            started: Instant::now(),
        })
    }

    pub fn snapshot(&self) -> RouterDriverSnapshot {
        RouterDriverSnapshot {
            wiring_cache: self.cache.snapshot(),
        }
    }

    /// Prepare the released components required by synchronous attachments.
    /// Native readiness initializes each exact component without invoking its handler.
    pub(crate) async fn prepare_synchronous_release(
        &self,
    ) -> anyhow::Result<PreparedReleaseReadiness> {
        let prepare_started = Instant::now();
        let manifest = self.release.manifest();
        let targets = synchronous_wiring_targets(manifest);
        anyhow::ensure!(
            targets.len() <= self.config.cache_capacity.get().get(),
            "release-wiring-preload-exceeds-cache-capacity"
        );
        let mut components = None;
        for (package_id, wiring_id, wiring_version) in &targets {
            let request = RouterDriverRequest {
                tenant_id: manifest.release.tenant_id.clone(),
                package_id: package_id.clone(),
                environment: manifest.release.environment.clone(),
                wiring_id: wiring_id.clone(),
                wiring_version: *wiring_version,
                delivery_id: format!("preload:{wiring_id}:{wiring_version}"),
                payload: serde_json::Value::Null,
                caller_attached: false,
                resolution: WiringResolution::Frozen,
                caller: None,
                traceparent: None,
                tracestate: None,
            };
            let active = self.resolve_frozen(&request).await.with_context(|| {
                format!("preload release wiring {wiring_id:?} version {wiring_version}")
            })?;
            self.validate_wiring_closure(&request, &active)?;
            for component in active.facts.components.iter() {
                self.validate_release_component(component)?;
            }
            components = Some(Arc::clone(&active.facts.components));
        }
        let Some(components) = components else {
            return Ok(PreparedReleaseReadiness {
                synchronous_wirings: 0,
                component_digests: 0,
            });
        };
        let digests: Vec<_> = components
            .iter()
            .map(|component| component.component_digest.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let bindings_ready = self
            .postgres
            .release_component_bindings_ready(
                &self.config.project,
                &manifest.release.tenant_id,
                manifest.release.effective_release_id.get(),
                &manifest.release.environment,
                &digests,
            )
            .await?;
        anyhow::ensure!(bindings_ready, "release-component-requirement-unbound");
        let application = self.released_application(&components).await?;
        for id in application.workload.facts_by_component_id.keys() {
            let target = application
                .workload
                .resolved
                .dispatch_target(id, NATIVE_POLICY_ID)
                .await?;
            prepare_native(
                &target,
                tokio::time::Instant::now() + Duration::from_millis(bounded_node_deadline_ms(None)),
            )
            .await
            .with_context(|| format!("initialize native release component {id:?}"))?;
        }
        tracing::info!(
            target: "wamn::router",
            synchronous_wirings = targets.len(),
            component_digests = digests.len(),
            elapsed_ms = %prepare_started.elapsed().as_millis(),
            "synchronous release preload completed"
        );
        Ok(PreparedReleaseReadiness {
            synchronous_wirings: targets.len(),
            component_digests: digests.len(),
        })
    }

    /// Execute one direct or queued delivery through the same router and node
    /// invoker. The caller owns acting on the terminal verdict.
    pub async fn execute(&self, request: RouterDriverRequest) -> anyhow::Result<RouterDelivery> {
        self.execute_with_context(request, None).await
    }

    /// Execute one delivery with host-derived event provenance.
    ///
    /// Only the router-delivery bridge can mint this context. It is distinct
    /// from caller identity: a post-commit registration remains callerless
    /// while every PostgreSQL transaction it drives carries the delivery's
    /// causation stamp.
    pub(crate) async fn execute_with_causation(
        &self,
        request: RouterDriverRequest,
        causation: Causation,
    ) -> anyhow::Result<RouterDelivery> {
        self.execute_with_context(request, Some(causation)).await
    }

    async fn execute_with_context(
        &self,
        request: RouterDriverRequest,
        causation: Option<Causation>,
    ) -> anyhow::Result<RouterDelivery> {
        self.validate_request_scope(&request)?;
        let active = self
            .resolve(&request)
            .instrument(tracing::info_span!("wamn.router.resolve"))
            .await?;
        self.validate_wiring_closure(&request, &active)?;
        self.execute_resolved(request, active, ExecutionClosure::Released, causation)
            .await
    }

    /// Execute a DB-frozen candidate through the same router and invoker as
    /// release-backed delivery.
    pub async fn execute_candidate(
        &self,
        request: CandidateCaseRequest,
    ) -> anyhow::Result<RouterDelivery> {
        self.validate_candidate_target(&request.target)?;
        let active = self
            .resolve_candidate(&request.target, &request.binding_world)
            .instrument(tracing::info_span!("wamn.router.resolve"))
            .await?;
        self.validate_candidate_closure(&request.target, &active)?;
        let component_bytes = self.fetch_candidate_components(&active).await?;
        let native = active
            .facts
            .components
            .iter()
            .map(|fact| {
                let bytes = component_bytes
                    .get(&fact.component_digest)
                    .context("candidate-component-bytes-missing")?;
                anyhow::Ok(NativeComponent {
                    fact: fact.clone(),
                    bytes: bytes.clone(),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let application = self.load_application(native).await?;
        let target = &request.target;
        let driver_request = RouterDriverRequest {
            tenant_id: target.tenant_id.clone(),
            package_id: target.package_id.clone(),
            environment: target.environment.clone(),
            wiring_id: target.wiring_id.clone(),
            wiring_version: target.wiring_version,
            delivery_id: request.delivery_id,
            payload: request.payload,
            // A management case expects `respond` as a terminal result, but it
            // has no synchronous durable caller. The queue adapter keeps those
            // two facts separate when persisting the outcome.
            caller_attached: true,
            resolution: WiringResolution::Frozen,
            caller: None,
            traceparent: request.traceparent,
            tracestate: request.tracestate,
        };
        let result = self
            .execute_resolved(
                driver_request,
                active,
                ExecutionClosure::Candidate {
                    target,
                    binding_world: &request.binding_world,
                    application: &application,
                },
                None,
            )
            .await;
        let cleanup = application.workload.resolved.unbind_all_plugins().await;
        match (result, cleanup) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error.context("unbind candidate native application")),
            (Ok(delivery), Ok(())) => Ok(delivery),
        }
    }

    async fn execute_resolved(
        &self,
        request: RouterDriverRequest,
        active: ActiveWiring<CatalogFacts>,
        closure: ExecutionClosure<'_>,
        causation: Option<Causation>,
    ) -> anyhow::Result<RouterDelivery> {
        // Parse the ingress context once per delivery, not once per node on the
        // router hot path. Queue delivery deliberately carries no remote
        // context and inherits the executor's host-created queue root instead.
        let remote_parent = remote_trace_context(&request);
        let wiring = Arc::clone(&active.wiring);
        let mut walk = wiring.start(Delivery {
            id: request.delivery_id.clone(),
            payload: request.payload.clone(),
            caller_attached: request.caller_attached,
        });
        let mut response =
            ResponseState::new(active.facts.response.as_deref(), request.caller_attached);
        loop {
            let now_ms = self.now_ms();
            match wiring.next(&mut walk, now_ms) {
                Step::Done(status) => {
                    let partial = if matches!(
                        status,
                        wamn_router::WalkStatus::Failed | wamn_router::WalkStatus::Cancelled
                    ) {
                        response.evidence(status, walk.failure(), walk.verdict())
                    } else {
                        None
                    };
                    return Ok(RouterDelivery {
                        wiring_version: active.version,
                        graph_hash: Arc::clone(&active.graph_hash),
                        partial,
                        outcome: Outcome {
                            status,
                            result: walk.result().clone(),
                            failure: walk.failure().cloned(),
                            hops: walk.hops(),
                            verdict: walk.verdict().cloned(),
                        },
                    });
                }
                Step::Wait { until_ms, .. } => {
                    let remaining = until_ms.saturating_sub(self.now_ms());
                    tokio::time::sleep(Duration::from_millis(remaining)).await;
                }
                Step::Invoke(call) => {
                    let effects = response.effect_evidence();
                    let result = async {
                        let component = active
                            .facts
                            .component(&call.node)
                            .ok_or_else(|| anyhow::anyhow!("router-node-component-fact-missing"))?;
                        let operation = component
                            .operation(&call.operation)
                            .ok_or_else(|| anyhow::anyhow!("router-node-operation-fact-missing"))?;
                        authorize_registered_operation(
                            request.caller.as_ref(),
                            operation.registered_operation.as_deref(),
                            operation.fresh_only,
                        )?;
                        let span = component_invocation_span(
                            &request,
                            &self.config.project,
                            active.version,
                            &component.component_digest,
                            &call,
                            remote_parent.as_ref(),
                        );
                        let outcome = self
                            .invoke_node(
                                &request,
                                &active,
                                &call,
                                closure,
                                causation.as_ref(),
                                effects.clone(),
                            )
                            .instrument(span)
                            .await
                            .with_context(|| format!("invoke wiring node {:?}", call.node))?;
                        response.observe(&call.node, &outcome, effects.as_ref())?;
                        anyhow::Ok(outcome)
                    }
                    .await;
                    let outcome = match result {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            if walk.verdict().is_some() {
                                tracing::warn!(error = %format_args!("{error:#}"), "router invocation failed after its terminal verdict; first verdict stands");
                                return Ok(RouterDelivery {
                                    wiring_version: active.version,
                                    graph_hash: Arc::clone(&active.graph_hash),
                                    partial: None,
                                    outcome: Outcome {
                                        status: wamn_router::WalkStatus::Failed,
                                        result: walk.result().clone(),
                                        failure: None,
                                        hops: walk.hops(),
                                        verdict: walk.verdict().cloned(),
                                    },
                                });
                            }
                            return Err(response.interrupted(error, effects.as_ref()));
                        }
                    };
                    if let Err(refusal) = wiring.apply(&mut walk, &call, outcome, self.now_ms()) {
                        wiring
                            .fail_on_node_data(&mut walk, &call.node, refusal)
                            .context("router driver applied an impossible transition")?;
                    }
                }
            }
        }
    }

    async fn resolve_candidate(
        &self,
        target: &CandidateWiringTarget,
        expected_binding_world: &CandidateBindingWorld,
    ) -> anyhow::Result<ActiveWiring<CatalogFacts>> {
        let resolved = self
            .postgres
            .resolve_candidate_wiring(
                &self.config.project,
                &target.tenant_id,
                &target.package_id,
                &target.environment,
                target.effective_release_id,
                &target.wiring_id,
                target.wiring_version,
                &target.wiring_hash,
                expected_binding_world,
            )
            .await?;
        let resolved = match resolved {
            CandidateWiringResolution::Resolved(resolved) => resolved,
            CandidateWiringResolution::Missing => {
                return Err(CandidateExecutionRefusal::new(
                    CandidateExecutionRefusalKind::Identity,
                    "candidate-wiring-not-found",
                )
                .into());
            }
            CandidateWiringResolution::InvalidDefinition => {
                return Err(CandidateExecutionRefusal::new(
                    CandidateExecutionRefusalKind::Definition,
                    "candidate-definition-invalid",
                )
                .into());
            }
            CandidateWiringResolution::BindingWorldUnavailable => {
                return Err(CandidateExecutionRefusal::new(
                    CandidateExecutionRefusalKind::Binding,
                    "candidate-binding-world-unavailable",
                )
                .into());
            }
            CandidateWiringResolution::BindingWorldDrift => {
                return Err(CandidateExecutionRefusal::new(
                    CandidateExecutionRefusalKind::Binding,
                    "candidate-binding-world-drift",
                )
                .into());
            }
        };
        let facts = CatalogFacts::from_resolved(&resolved)?;
        if let Some(active) = self.cache.get_version(
            &target.tenant_id,
            &target.package_id,
            &target.environment,
            target.effective_release_id,
            &target.wiring_id,
            target.wiring_version,
        ) {
            if active.graph_hash != resolved.graph_hash || active.facts.as_ref() != &facts {
                return Err(CandidateExecutionRefusal::new(
                    CandidateExecutionRefusalKind::Identity,
                    "candidate-wiring-immutable-hash-mismatch",
                )
                .into());
            }
            return Ok(active);
        }
        match self.cache.insert_version(
            &target.tenant_id,
            &target.package_id,
            &target.environment,
            target.effective_release_id,
            &target.wiring_id,
            resolved.version,
            Arc::clone(&resolved.graph_hash),
            resolved.wiring,
            facts,
        ) {
            CacheInsert::Installed(active) => Ok(active),
            CacheInsert::HashMismatch => Err(CandidateExecutionRefusal::new(
                CandidateExecutionRefusalKind::Identity,
                "candidate-wiring-immutable-hash-mismatch",
            )
            .into()),
            CacheInsert::Overtaken => unreachable!("exact-version insert has no pointer token"),
        }
    }

    async fn resolve(
        &self,
        request: &RouterDriverRequest,
    ) -> anyhow::Result<ActiveWiring<CatalogFacts>> {
        match request.resolution {
            WiringResolution::Active => self.resolve_active(request).await,
            WiringResolution::Frozen => self.resolve_frozen(request).await,
        }
    }

    async fn resolve_active(
        &self,
        request: &RouterDriverRequest,
    ) -> anyhow::Result<ActiveWiring<CatalogFacts>> {
        let mounted_effective_release_id =
            self.release.manifest().release.effective_release_id.get();
        loop {
            let token = match self.cache.get(
                &request.tenant_id,
                &request.package_id,
                &request.environment,
                mounted_effective_release_id,
                &request.wiring_id,
            ) {
                Lookup::Hit(active) if active.version == request.wiring_version => {
                    return Ok(active);
                }
                Lookup::Hit(_) => {
                    self.cache.invalidate(
                        &request.tenant_id,
                        &request.package_id,
                        &request.environment,
                        &request.wiring_id,
                    );
                    continue;
                }
                Lookup::Miss(token) => token,
            };
            let resolved = self
                .postgres
                .resolve_active_wiring(
                    &self.config.project,
                    &request.tenant_id,
                    &request.package_id,
                    &request.environment,
                    &request.wiring_id,
                    request.wiring_version,
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("active-wiring-not-found"))?;
            anyhow::ensure!(
                resolved.effective_release_id == mounted_effective_release_id,
                "active-wiring-effective-release-mismatch"
            );
            let facts = CatalogFacts::from_resolved(&resolved)?;
            match self.cache.insert(
                &request.tenant_id,
                &request.package_id,
                &request.environment,
                mounted_effective_release_id,
                &request.wiring_id,
                resolved.version,
                Arc::clone(&resolved.graph_hash),
                resolved.wiring,
                facts,
                token,
            ) {
                CacheInsert::Installed(active) => return Ok(active),
                CacheInsert::Overtaken => continue,
                CacheInsert::HashMismatch => {
                    anyhow::bail!("active-wiring-immutable-hash-mismatch")
                }
            }
        }
    }

    async fn resolve_frozen(
        &self,
        request: &RouterDriverRequest,
    ) -> anyhow::Result<ActiveWiring<CatalogFacts>> {
        let effective_release_id = self.release.manifest().release.effective_release_id.get();
        if let Some(active) = self.cache.get_version(
            &request.tenant_id,
            &request.package_id,
            &request.environment,
            effective_release_id,
            &request.wiring_id,
            request.wiring_version,
        ) {
            return Ok(active);
        }
        let resolved = self
            .postgres
            .resolve_release_wiring(
                &self.config.project,
                &request.tenant_id,
                &request.package_id,
                &request.environment,
                effective_release_id,
                self.release.release().manifest_digest.as_str(),
                &request.wiring_id,
                request.wiring_version,
            )
            .await?
            .ok_or_else(|| anyhow::anyhow!("release-wiring-not-found"))?;
        let facts = CatalogFacts::from_resolved(&resolved)?;
        match self.cache.insert_version(
            &request.tenant_id,
            &request.package_id,
            &request.environment,
            effective_release_id,
            &request.wiring_id,
            resolved.version,
            Arc::clone(&resolved.graph_hash),
            resolved.wiring,
            facts,
        ) {
            CacheInsert::Installed(active) => Ok(active),
            CacheInsert::HashMismatch => {
                anyhow::bail!("release-wiring-immutable-hash-mismatch")
            }
            CacheInsert::Overtaken => unreachable!("exact-version insert has no pointer token"),
        }
    }

    fn validate_request_scope(&self, request: &RouterDriverRequest) -> anyhow::Result<()> {
        let release = &self.release.manifest().release;
        anyhow::ensure!(request.wiring_version > 0, "wiring-version-zero");
        anyhow::ensure!(
            release.tenant_id == request.tenant_id
                && release.environment == request.environment
                && release
                    .packages
                    .iter()
                    .any(|package| package.package_id() == request.package_id),
            "router-request-release-scope-mismatch"
        );
        Ok(())
    }

    fn validate_candidate_target(
        &self,
        target: &CandidateWiringTarget,
    ) -> Result<(), CandidateExecutionRefusal> {
        let release = &self.release.manifest().release;
        if target.effective_release_id == 0 || target.wiring_version == 0 {
            return Err(CandidateExecutionRefusal::new(
                CandidateExecutionRefusalKind::Identity,
                "candidate-wiring-coordinate-incomplete",
            ));
        }
        if release.tenant_id != target.tenant_id
            || release.environment != target.environment
            || !release
                .packages
                .iter()
                .any(|package| package.package_id() == target.package_id)
        {
            return Err(CandidateExecutionRefusal::new(
                CandidateExecutionRefusalKind::Identity,
                "candidate-request-release-scope-mismatch",
            ));
        }
        if target.wiring_hash.is_empty() {
            return Err(CandidateExecutionRefusal::new(
                CandidateExecutionRefusalKind::Identity,
                "candidate-wiring-coordinate-incomplete",
            ));
        }
        Ok(())
    }

    fn validate_candidate_closure(
        &self,
        target: &CandidateWiringTarget,
        active: &ActiveWiring<CatalogFacts>,
    ) -> Result<(), CandidateExecutionRefusal> {
        if active.version != target.wiring_version
            || active.graph_hash.as_ref() != target.wiring_hash
            || active.facts.effective_release_id != target.effective_release_id
            || active.facts.components.iter().any(|component| {
                component.scope.tenant_id != target.tenant_id
                    || component.scope.package_id != target.package_id
            })
        {
            return Err(CandidateExecutionRefusal::new(
                CandidateExecutionRefusalKind::Definition,
                "candidate-wiring-closure-mismatch",
            ));
        }
        Ok(())
    }

    async fn fetch_candidate_components(
        &self,
        active: &ActiveWiring<CatalogFacts>,
    ) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
        let mut bytes_by_digest = BTreeMap::new();
        for component in active.facts.components.iter() {
            match self.source.pull_verified(component).await {
                Ok(bytes) => {
                    bytes_by_digest.insert(component.component_digest.clone(), bytes);
                }
                Err(error) if error.kind() == ComponentArtifactFetchErrorKind::Unavailable => {
                    return Err(error.into());
                }
                Err(error) => {
                    return Err(CandidateExecutionRefusal::new(
                        CandidateExecutionRefusalKind::Artifact,
                        error.refusal(),
                    )
                    .into());
                }
            }
        }
        Ok(bytes_by_digest)
    }

    fn validate_wiring_closure(
        &self,
        request: &RouterDriverRequest,
        active: &ActiveWiring<CatalogFacts>,
    ) -> anyhow::Result<()> {
        let expected = ServingWiring {
            package_id: request.package_id.clone(),
            wiring_id: request.wiring_id.clone(),
            wiring_version: request.wiring_version,
            graph_hash: DefinitionHash::parse(active.graph_hash.as_ref())
                .context("active wiring carries a non-canonical definition hash")?,
        };
        anyhow::ensure!(
            self.release.manifest().wirings.contains(&expected),
            "wiring-not-in-carried-release"
        );
        anyhow::ensure!(
            active.facts.effective_release_id
                == self.release.manifest().release.effective_release_id.get(),
            "wiring-effective-release-not-carried"
        );
        Ok(())
    }

    fn validate_release_component(&self, component: &AdmittedComponent) -> anyhow::Result<()> {
        validate_component_in_release(&self.release, component)
    }

    async fn released_application(
        &self,
        components: &[AdmittedComponent],
    ) -> anyhow::Result<Arc<NativeApplication>> {
        for component in components {
            self.validate_release_component(component)?;
        }
        let application = self
            .native
            .get_or_try_init(|| async {
                let pulls = components.iter().cloned().map(|fact| {
                    let source = self.source.clone();
                    async move {
                        let bytes = source
                            .pull_verified(&fact)
                            .instrument(tracing::info_span!(
                                "wamn.component.pull",
                                wamn.component_digest = %fact.component_digest,
                            ))
                            .await?;
                        anyhow::Ok(NativeComponent { fact, bytes })
                    }
                });
                let mut pulls = stream::iter(pulls).buffered(COMPONENT_FETCH_CONCURRENCY);
                let mut native = Vec::with_capacity(components.len());
                while let Some(component) = pulls.next().await {
                    native.push(component?);
                }
                self.load_application(native).await
            })
            .await?;
        let loaded = &application.workload.facts_by_component_id;
        anyhow::ensure!(
            components.len() == loaded.len()
                && components
                    .iter()
                    .all(|fact| loaded.values().any(|loaded| loaded == fact)),
            "native-release-component-closure-mismatch"
        );
        Ok(Arc::clone(application))
    }

    async fn load_application(
        &self,
        components: Vec<NativeComponent>,
    ) -> anyhow::Result<Arc<NativeApplication>> {
        let facts: Vec<_> = components
            .iter()
            .map(|component| component.fact.clone())
            .collect();
        let policy = new_native_policy(
            &facts,
            NativePolicyResources {
                postgres: Arc::clone(&self.postgres),
                logging: Arc::clone(&self.logging),
                connection_http: Arc::new(ConnectionHttp::new(
                    Arc::clone(&self.postgres),
                    Arc::clone(&self.http_transport),
                    Arc::clone(&self.credentials),
                    self.release.manifest().release.tenant_id.as_str(),
                    self.config.project.as_str(),
                    Arc::clone(&self.allowed_hosts),
                    Some(Arc::clone(&self.release)),
                )),
                blobstore: Arc::new(WamnBlobstore::new(
                    Arc::clone(&self.postgres),
                    Arc::clone(&self.credentials),
                    self.release.manifest().release.tenant_id.as_str(),
                    self.config.project.as_str(),
                    Some(Arc::clone(&self.release)),
                )),
                release: Arc::clone(&self.release),
                project: self.config.project.clone(),
            },
        )?;
        let world = policy.world();
        let host_interfaces = world
            .imports
            .into_iter()
            .chain(world.exports)
            .chain(facts.iter().flat_map(|fact| {
                fact.imports
                    .iter()
                    .map(|name| WitInterface::from(name.as_str()))
            }))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let plugins: HashMap<&'static str, Arc<dyn HostPlugin>> =
            HashMap::from([(NATIVE_POLICY_ID, Arc::clone(&policy) as Arc<dyn HostPlugin>)]);
        load_native_application(
            Arc::clone(&self.engine),
            NativeWorkloadSpec {
                id: next_scope("wamn-application").into(),
                namespace: self.config.project.clone(),
                name: self.config.owner_prefix.clone(),
                components,
                local_resources: wash_runtime::types::LocalResources::default(),
                host_interfaces,
            },
            policy,
            &plugins,
            &wash_runtime::plugin::PluginBindings::new(),
            &wash_runtime::observability::Meters::new(
                wash_runtime::observability::MeterKind::Duration,
            ),
        )
        .await
    }

    async fn invoke_node(
        &self,
        request: &RouterDriverRequest,
        active: &ActiveWiring<CatalogFacts>,
        call: &wamn_router::NodeCall,
        closure: ExecutionClosure<'_>,
        causation: Option<&Causation>,
        effects: Option<EffectEvidence>,
    ) -> anyhow::Result<NodeOutcome> {
        let component = active
            .facts
            .component(&call.node)
            .ok_or_else(|| anyhow::anyhow!("router-node-component-fact-missing"))?;
        if matches!(closure, ExecutionClosure::Released) {
            self.validate_release_component(component)?;
        }
        let release = if matches!(closure, ExecutionClosure::Released) {
            Some(ReleaseIdentity {
                effective_release_id: self.release.release().effective_release_id,
                manifest_digest: self.release.release().manifest_digest.clone(),
            })
        } else {
            None
        };
        let connection_closure = match closure {
            ExecutionClosure::Released => ConnectionExecutionClosure::Released,
            ExecutionClosure::Candidate {
                target,
                binding_world,
                ..
            } => ConnectionExecutionClosure::Candidate {
                effective_release_id: target.effective_release_id,
                environment: target.environment.clone(),
                wiring_hash: target.wiring_hash.clone(),
                component: component.component.clone(),
                interface_version: component.interface_version.clone(),
                binding_world: Arc::clone(binding_world),
            },
        };
        let acquisition = NodeAcquisition {
            claims: SessionClaims {
                tenant: request.tenant_id.clone(),
                project: Some(self.config.project.clone()),
                schema: self.config.schema.clone(),
                runner: Some(self.config.owner_prefix.clone()),
                role: None,
                user_id: None,
                release,
            },
            invocation: ConnectionInvocation {
                origin: ConnectionOrigin {
                    wiring_package_id: request.package_id.clone(),
                    package_id: component.scope.package_id.clone(),
                    component_digest: component.component_digest.clone(),
                    component: component.component.clone(),
                    interface_version: component.interface_version.clone(),
                    operation: call.operation.clone(),
                },
                package_id: component.scope.package_id.clone(),
                wiring_id: request.wiring_id.clone(),
                wiring_version: active.version,
                node_id: call.node.clone(),
                occurrence: call.occurrence,
                component_digest: component.component_digest.clone(),
                // The admitted component name, read off the catalog fact this
                // node resolved to. The per-request pooled scope is an instance
                // id and names no component a reader can look up, so an effect
                // span takes its component identity from here (`wamn-b2m6.7`).
                component: component.component.clone(),
                operation: call.operation.clone(),
                closure: connection_closure,
                effects,
            },
            causation: causation.cloned(),
        };
        let deadline_ms = bounded_node_deadline_ms(call.deadline_ms);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(deadline_ms);
        tokio::time::timeout_at(deadline, async {
            let application = match closure {
                ExecutionClosure::Released => {
                    self.released_application(&active.facts.components).await?
                }
                ExecutionClosure::Candidate { application, .. } => Arc::clone(application),
            };
            let id = application
                .workload
                .facts_by_component_id
                .iter()
                .find_map(|(id, fact)| (fact == component).then_some(id))
                .context("native-node-component-fact-missing")?;
            let target = application
                .workload
                .resolved
                .dispatch_target(id, NATIVE_POLICY_ID)
                .await?;
            let context = node_context(request, active.version, call, deadline_ms)?;
            let input = serde_json::to_string(&call.payload).context("encode node input")?;
            invoke_native(
                &target,
                NativeInvocation {
                    operation: call.operation.clone(),
                    context,
                    input,
                    deadline,
                    acquisition,
                    caller: request.caller.clone(),
                    application,
                },
            )
            .await
            .and_then(lower_node_outcome)
        })
        .await
        .context("native node enclosing deadline elapsed")?
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

fn validate_component_in_release(
    release: &ReleaseManifestWeld,
    component: &AdmittedComponent,
) -> anyhow::Result<()> {
    let manifest = release.manifest();
    let package_version = manifest
        .release
        .packages
        .iter()
        .find(|package| package.package_id() == component.scope.package_id)
        .map(|package| package.package_version());
    anyhow::ensure!(
        component.scope.tenant_id == manifest.release.tenant_id
            && package_version == Some(component.scope.package_version.as_str()),
        "release-component-scope-mismatch"
    );
    let expected = ServingComponent {
        package_id: component.scope.package_id.clone(),
        component: component.component.clone(),
        interface_version: component.interface_version.clone(),
        digest: ArtifactHash::parse(component.component_digest.clone())
            .context("component fact carries a non-canonical artifact hash")?,
        operations: component
            .operations
            .iter()
            .map(|(name, operation)| {
                (
                    name.clone(),
                    ServingComponentOperation {
                        registered_operation: operation.registered_operation.clone(),
                        fresh_only: operation.fresh_only,
                        committed_result_schema: operation.committed_result_schema.as_ref().map(
                            |schema| {
                                String::from_utf8(wamn_execution_contract::canonical_json_bytes(
                                    &schema.schema,
                                ))
                                .expect("canonical JSON is UTF-8")
                            },
                        ),
                        dependencies: operation.dependencies.clone(),
                        statements: operation.statements.clone(),
                    },
                )
            })
            .collect(),
    };
    anyhow::ensure!(
        manifest.components.contains(&expected),
        "component-not-in-carried-release"
    );
    Ok(())
}

fn synchronous_wiring_targets(manifest: &ServingManifest) -> BTreeSet<(String, String, u32)> {
    manifest
        .attachments
        .values()
        .filter(|attachment| synchronous_request_kind(attachment.kind))
        .map(|attachment| {
            (
                attachment.package_id.clone(),
                attachment.wiring_id.clone(),
                attachment.wiring_version,
            )
        })
        .collect()
}

fn synchronous_request_kind(kind: AttachmentKind) -> bool {
    matches!(
        kind,
        AttachmentKind::Http | AttachmentKind::Internal | AttachmentKind::Studio
    )
}

#[derive(Debug, Clone)]
struct NodeAcquisition {
    claims: SessionClaims,
    invocation: ConnectionInvocation,
    /// Event provenance for every transaction opened during this acquisition.
    /// This is intentionally independent of `caller`: post-commit delivery has
    /// causation but no caller identity.
    causation: Option<Causation>,
}

impl NodeAcquisition {
    /// Point one acquisition at the nested target it is about to enter.
    ///
    /// The target arrives as the catalog fact rather than as its parts, because
    /// three of the four retargeted values are read off one fact and four bare
    /// strings let a caller swap two of them with no type error.
    ///
    /// The component name and the operation move with the package and the
    /// digest. A child that kept the parent's pair would raise its effects under
    /// the caller's identity while naming its own package (`wamn-b2m6.7`).
    /// The original wiring owner and root component remain in `origin`.
    fn retarget(mut self, target: &AdmittedComponent, operation: &str) -> Self {
        self.invocation.package_id = target.scope.package_id.clone();
        self.invocation.component_digest = target.component_digest.clone();
        self.invocation.component = target.component.clone();
        self.invocation.operation = operation.to_owned();
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NestedOperationRefusalKind {
    IdentityUnbound,
    UndeclaredForExport,
    ReleaseClosureUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NestedOperationRefusal {
    kind: NestedOperationRefusalKind,
    operation: Box<str>,
}

impl NestedOperationRefusal {
    fn new(kind: NestedOperationRefusalKind, operation: &str) -> Self {
        Self {
            kind,
            operation: operation.into(),
        }
    }

    fn literal(&self) -> &'static str {
        match self.kind {
            NestedOperationRefusalKind::IdentityUnbound => "nested-operation-identity-unbound",
            NestedOperationRefusalKind::UndeclaredForExport => {
                "nested-operation-not-declared-for-export"
            }
            NestedOperationRefusalKind::ReleaseClosureUnavailable => {
                "nested-operation-release-closure-unavailable"
            }
        }
    }
}

impl fmt::Display for NestedOperationRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.literal(), self.operation)
    }
}

impl std::error::Error for NestedOperationRefusal {}

fn nested_host_error(error: anyhow::Error) -> wash_runtime::wasmtime::Error {
    if let Some(denial) = error.downcast_ref::<OperationRefusal>() {
        return wash_runtime::wasmtime::Error::new(denial.clone());
    }
    if let Some(refusal) = error.downcast_ref::<NestedOperationRefusal>() {
        return wash_runtime::wasmtime::Error::new(refusal.clone());
    }
    wash_runtime::wasmtime::Error::msg(format!("{error:#}"))
}

/// Dependency operation -> (its exact pin, the owner operations that import it).
type NestedOperationLinks = BTreeMap<String, (ComponentOperationDependency, BTreeSet<String>)>;

fn nested_operation_links(component: &AdmittedComponent) -> anyhow::Result<NestedOperationLinks> {
    let mut links = NestedOperationLinks::new();
    for (owner_operation, operation) in &component.operations {
        for dependency in &operation.dependencies {
            if links
                .get(&dependency.operation)
                .is_some_and(|(pinned, _)| pinned != dependency)
            {
                anyhow::bail!("component-operation-dependency-pin-mismatch");
            }
            let (_, owners) = links
                .entry(dependency.operation.clone())
                .or_insert_with(|| (dependency.clone(), BTreeSet::new()));
            owners.insert(owner_operation.clone());
        }
    }
    Ok(links)
}

fn lower_statement_value_type(value_type: ComponentSqlValueType) -> StatementValueType {
    match value_type {
        ComponentSqlValueType::Boolean => StatementValueType::Boolean,
        ComponentSqlValueType::Int32 => StatementValueType::Int32,
        ComponentSqlValueType::Int64 => StatementValueType::Int64,
        ComponentSqlValueType::Float64 => StatementValueType::Float64,
        ComponentSqlValueType::Text => StatementValueType::Text,
        ComponentSqlValueType::Bytes => StatementValueType::Bytes,
        ComponentSqlValueType::Numeric => StatementValueType::Numeric,
        ComponentSqlValueType::Timestamptz => StatementValueType::Timestamptz,
        ComponentSqlValueType::Json => StatementValueType::Json,
        ComponentSqlValueType::Uuid => StatementValueType::Uuid,
    }
}

fn lower_statement_field(field: &ComponentSqlField) -> StatementField {
    StatementField {
        value_type: lower_statement_value_type(field.value_type),
        nullable: field.nullable,
    }
}

/// Lower and verify every operation's statement set out of the admitted facts.
/// The complete admitted fact owns these immutable statements.
fn prepare_statement_sets(
    component: &AdmittedComponent,
) -> anyhow::Result<BTreeMap<String, PreparedStatementSet>> {
    component
        .operations
        .iter()
        .map(|(operation, fact)| {
            WamnPostgres::prepare_statement_set(lower_statement_set(&fact.statements))
                .with_context(|| format!("prepare verified statements for operation {operation:?}"))
                .map(|prepared| (operation.clone(), prepared))
        })
        .collect()
}

fn lower_statement_set(
    statements: &BTreeMap<String, wamn_catalog::ComponentSqlStatement>,
) -> VerifiedStatementSet {
    statements
        .iter()
        .map(|(digest, statement)| {
            (
                digest.clone(),
                VerifiedStatement {
                    exact_sql: statement.sql.clone().into_boxed_str(),
                    binds: statement.binds.iter().map(lower_statement_field).collect(),
                    columns: statement
                        .columns
                        .iter()
                        .map(lower_statement_field)
                        .collect(),
                    transactional: statement.transactional,
                },
            )
        })
        .collect()
}

/// Unique process-local application and invocation scope identifiers.
static NEXT_SCOPE: AtomicU64 = AtomicU64::new(0);

fn next_scope(component: &str) -> Box<str> {
    format!("{component}#{}", NEXT_SCOPE.fetch_add(1, Ordering::Relaxed)).into()
}

fn bounded_node_deadline_ms(deadline_ms: Option<u64>) -> u64 {
    deadline_ms
        .unwrap_or(MAX_HOST_CALL_DURATION.as_millis() as u64)
        .clamp(1, MAX_HOST_CALL_DURATION.as_millis() as u64)
}

fn lower_node_outcome(
    outcome: Result<node_types::Emission, node_types::NodeError>,
) -> anyhow::Result<NodeOutcome> {
    match outcome {
        Ok(emission) => {
            let payload = serde_json::from_str(&emission.payload)
                .context("wamn:node emitted invalid JSON")?;
            Ok(NodeOutcome::Success {
                payload,
                port: emission
                    .port
                    .unwrap_or_else(|| wamn_router::MAIN_PORT.to_owned()),
            })
        }
        Err(node_types::NodeError::Retryable(detail)) => Ok(NodeOutcome::Error(
            NodeError::Retryable(lower_detail(detail)),
        )),
        Err(node_types::NodeError::RateLimited(detail)) => Ok(NodeOutcome::Error(
            NodeError::RateLimited(RateLimitDetail {
                detail: lower_detail(detail.detail),
                retry_after_ms: detail.retry_after_ms,
                target_host: None,
            }),
        )),
        Err(node_types::NodeError::Terminal(detail)) => Ok(NodeOutcome::Error(
            NodeError::Terminal(lower_detail(detail)),
        )),
        Err(node_types::NodeError::InvalidInput(detail)) => Ok(NodeOutcome::Error(
            NodeError::InvalidInput(lower_detail(detail)),
        )),
        Err(node_types::NodeError::Cancelled) => Ok(NodeOutcome::Cancelled),
    }
}

fn lower_detail(detail: node_types::ErrorDetail) -> ErrorDetail {
    ErrorDetail {
        message: detail.message,
        code: detail.code,
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use opentelemetry_sdk::trace::{
        InMemorySpanExporter, InMemorySpanExporterBuilder, SdkTracerProvider, SpanData,
    };
    use tracing_subscriber::layer::SubscriberExt as _;
    use wamn_catalog::{
        AdmittedComponentEffect, AdmittedComponentOperation, ComponentPackageScope,
        EffectiveReleaseId, PackageCoordinate, SERVING_MANIFEST_FORMAT_VERSION, ServingAttachment,
        ServingRegistration, ServingRegistrationInput, ServingRelease,
    };

    use super::*;

    const TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const PARENT_SPAN_ID: &str = "00f067aa0ba902b7";
    const VALID_TRACEPARENT: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    fn component_with_operations(
        operations: BTreeMap<String, AdmittedComponentOperation>,
    ) -> AdmittedComponent {
        AdmittedComponent {
            scope: ComponentPackageScope {
                tenant_id: "tenant-a".to_owned(),
                package_id: "orders".to_owned(),
                package_version: "1.0.0".to_owned(),
            },
            component: "orders".to_owned(),
            interface_version: "0.1.0".to_owned(),
            operations,
            component_digest:
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            imports: Vec::new(),
            imports_fingerprint:
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            effects: Vec::new(),
        }
    }

    fn operation_with_statements(
        statements: BTreeMap<String, wamn_catalog::ComponentSqlStatement>,
    ) -> AdmittedComponentOperation {
        AdmittedComponentOperation {
            registered_operation: None,
            fresh_only: false,
            committed_result_schema: None,
            dependencies: Vec::new(),
            input_ports: Vec::new(),
            output_ports: Vec::new(),
            parameters: Vec::new(),
            statements,
        }
    }

    fn statement_plugin() -> Arc<WamnPostgres> {
        Arc::new(WamnPostgres::with_provider(Arc::new(
            wamn_runtime::plugins::wamn_postgres::StaticCredentialProvider::new(
                HashMap::new(),
                None,
            ),
        )))
    }

    struct TraceHarness {
        exporter: InMemorySpanExporter,
        provider: SdkTracerProvider,
        _guard: tracing::subscriber::DefaultGuard,
    }

    impl TraceHarness {
        fn install() -> Self {
            opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
            let exporter = InMemorySpanExporterBuilder::new().build();
            let provider = SdkTracerProvider::builder()
                .with_simple_exporter(exporter.clone())
                .build();
            let subscriber = tracing_subscriber::registry().with(
                tracing_opentelemetry::layer().with_tracer(provider.tracer("router-span-test")),
            );
            let guard = tracing::subscriber::set_default(subscriber);
            Self {
                exporter,
                provider,
                _guard: guard,
            }
        }

        fn spans(&self) -> Vec<SpanData> {
            self.provider.force_flush().expect("test spans must flush");
            self.exporter
                .get_finished_spans()
                .expect("test span exporter must remain readable")
        }
    }

    fn driver_request(traceparent: Option<&str>) -> RouterDriverRequest {
        RouterDriverRequest {
            tenant_id: "tenant-a".to_owned(),
            package_id: "orders".to_owned(),
            environment: "prod".to_owned(),
            wiring_id: "route-order".to_owned(),
            wiring_version: 7,
            delivery_id: "delivery-9".to_owned(),
            payload: serde_json::json!({"id": 9}),
            caller_attached: true,
            resolution: WiringResolution::Frozen,
            caller: None,
            traceparent: traceparent.map(str::to_owned),
            tracestate: traceparent.map(|_| "vendor=value".to_owned()),
        }
    }

    fn node_call() -> wamn_router::NodeCall {
        wamn_router::NodeCall {
            node: "load-order".to_owned(),
            input_port: Some("request".to_owned()),
            component: "entity".to_owned(),
            operation: "orders:purchase-order/get@1.0.0".to_owned(),
            config: serde_json::json!({}),
            connection: None,
            credential: None,
            payload: serde_json::json!({"id": 9}),
            attempt: 0,
            occurrence: 0,
            deadline_ms: Some(100),
        }
    }

    fn span_named<'a>(spans: &'a [SpanData], name: &str) -> &'a SpanData {
        spans
            .iter()
            .find(|span| span.name == name)
            .unwrap_or_else(|| panic!("span {name:?} must be exported"))
    }

    fn attribute(span: &SpanData, key: &str) -> Option<String> {
        span.attributes
            .iter()
            .find(|attribute| attribute.key.as_str() == key)
            .map(|attribute| attribute.value.to_string())
    }

    fn attachment(kind: AttachmentKind, wiring_id: &str) -> ServingAttachment {
        ServingAttachment {
            kind,
            package_id: "orders".to_owned(),
            wiring_id: wiring_id.to_owned(),
            wiring_version: 3,
            definition_hash: DefinitionHash::parse(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .expect("fixture definition hash is canonical"),
            definition: serde_json::json!({}),
            auth_policy: serde_json::json!({}),
            registered_operation: None,
        }
    }

    #[test]
    fn node_deadline_is_nonzero_and_host_bounded() {
        let ceiling = MAX_HOST_CALL_DURATION.as_millis() as u64;

        assert_eq!(bounded_node_deadline_ms(None), ceiling);
        assert_eq!(bounded_node_deadline_ms(Some(0)), 1);
        assert_eq!(bounded_node_deadline_ms(Some(ceiling + 1)), ceiling);
        assert_eq!(bounded_node_deadline_ms(Some(17)), 17);
    }

    #[test]
    fn admitted_statement_types_lower_exhaustively_to_the_runtime_vocabulary() {
        let cases = [
            (ComponentSqlValueType::Boolean, StatementValueType::Boolean),
            (ComponentSqlValueType::Int32, StatementValueType::Int32),
            (ComponentSqlValueType::Int64, StatementValueType::Int64),
            (ComponentSqlValueType::Float64, StatementValueType::Float64),
            (ComponentSqlValueType::Text, StatementValueType::Text),
            (ComponentSqlValueType::Bytes, StatementValueType::Bytes),
            (ComponentSqlValueType::Numeric, StatementValueType::Numeric),
            (
                ComponentSqlValueType::Timestamptz,
                StatementValueType::Timestamptz,
            ),
            (ComponentSqlValueType::Json, StatementValueType::Json),
            (ComponentSqlValueType::Uuid, StatementValueType::Uuid),
        ];

        for (index, (admitted, runtime)) in cases.into_iter().enumerate() {
            let field = ComponentSqlField {
                name: format!("field-{index}"),
                value_type: admitted,
                nullable: index % 2 == 0,
            };
            assert_eq!(
                lower_statement_field(&field),
                StatementField {
                    value_type: runtime,
                    nullable: field.nullable,
                }
            );
        }
    }

    #[test]
    fn partial_statement_binding_failure_cleans_earlier_operations() {
        let postgres = statement_plugin();
        let invalid_statement = wamn_catalog::ComponentSqlStatement {
            name: "lookup".to_owned(),
            path: "sql/lookup.sql".to_owned(),
            sql: "SELECT 1".to_owned(),
            binds: Vec::new(),
            columns: Vec::new(),
            transactional: false,
        };
        let component = component_with_operations(BTreeMap::from([
            (
                "a-empty".to_owned(),
                operation_with_statements(BTreeMap::new()),
            ),
            (
                "b-invalid".to_owned(),
                operation_with_statements(BTreeMap::from([(
                    "sha256:not-the-statement-digest".to_owned(),
                    invalid_statement,
                )])),
            ),
        ]));

        // The refusal moved to preparation: a digest that does not name its
        // SQL is refused before any scope exists to bind it under, so nothing
        // partial can ever have been bound.
        let error = prepare_statement_sets(&component)
            .expect_err("a digest that does not name its SQL is refused at preparation");
        assert!(
            format!("{error:#}").contains("statement-digest-mismatch"),
            "the refusal names the mismatch: {error:#}"
        );
        assert!(
            postgres
                .activate_statement_operation("scope-partial", "a-empty")
                .is_err(),
            "no operation of a refused digest is ever bound"
        );
    }

    #[test]
    fn candidate_refusal_preserves_class_and_frozen_literal() {
        let refusal = CandidateExecutionRefusal::new(
            CandidateExecutionRefusalKind::Binding,
            "candidate-binding-world-drift",
        );
        assert_eq!(refusal.kind(), CandidateExecutionRefusalKind::Binding);
        assert_eq!(refusal.refusal(), "candidate-binding-world-drift");
        assert_eq!(refusal.to_string(), "candidate-binding-world-drift");
    }

    #[test]
    fn every_registered_invocation_requires_the_exact_operation_grant() {
        let operation = "orders:purchase-order/get@7.0.0";

        assert!(authorize_registered_operation(None, None, false).is_ok());
        let denial = authorize_registered_operation(None, Some(operation), false)
            .expect_err("a registered invocation without an originating caller is denied");
        assert_eq!(denial.operation(), operation);
    }

    #[test]
    fn nested_acquisition_preserves_causation_and_root_origin() {
        let causation = Causation {
            run: "registration:delivery:9".to_owned(),
            root: "attachment:delivery:1".to_owned(),
            depth: 2,
        };
        let acquisition = NodeAcquisition {
            claims: SessionClaims {
                tenant: "tenant-a".to_owned(),
                project: Some("project-a".to_owned()),
                schema: Some("app".to_owned()),
                runner: Some("executor-a".to_owned()),
                role: Some("operator".to_owned()),
                user_id: Some("user-a".to_owned()),
                release: Some(ReleaseIdentity {
                    effective_release_id: 7,
                    manifest_digest: wamn_catalog::ManifestDigest::parse(
                        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                    .expect("valid manifest digest"),
                }),
            },
            invocation: ConnectionInvocation {
                origin: ConnectionOrigin {
                    wiring_package_id: "org_workflow".to_owned(),
                    package_id: "client_acme_receiving".to_owned(),
                    component_digest: "sha256:overlay".to_owned(),
                    component: "overlay".to_owned(),
                    interface_version: "1.0.0".to_owned(),
                    operation: "client-acme-receiving:receiving/record-receipt@1.0.0".to_owned(),
                },
                package_id: "client_acme_receiving".to_owned(),
                wiring_id: "record-receipt".to_owned(),
                wiring_version: 1,
                node_id: "base-command".to_owned(),
                occurrence: 0,
                component_digest: "sha256:overlay".to_owned(),
                component: "overlay".to_owned(),
                operation: "client-acme-receiving:receiving/record-receipt@1.0.0".to_owned(),
                closure: ConnectionExecutionClosure::Released,
                effects: None,
            },
            causation: Some(causation.clone()),
        };

        let original = acquisition.clone();
        let mut target = component_with_operations(BTreeMap::new());
        target.scope.package_id = "wamn_receiving".to_owned();
        target.component = "receiving".to_owned();
        target.component_digest = "sha256:base".to_owned();
        let child = acquisition.retarget(&target, "wamn-receiving:receiving/record-receipt@1.0.0");
        assert_eq!(child.causation.as_ref(), Some(&causation));
        assert_eq!(child.invocation.package_id, "wamn_receiving");
        assert_eq!(child.invocation.component_digest, "sha256:base");
        assert_eq!(child.invocation.wiring_id, "record-receipt");
        // The child raises its effects under ITS OWN component and operation.
        // The overlay's pair belongs to the caller (`wamn-b2m6.7`).
        assert_eq!(child.invocation.component, "receiving");
        assert_eq!(
            child.invocation.operation,
            "wamn-receiving:receiving/record-receipt@1.0.0"
        );
        assert_eq!(child.claims, original.claims);
        assert_eq!(
            child.invocation,
            ConnectionInvocation {
                package_id: "wamn_receiving".to_owned(),
                component_digest: "sha256:base".to_owned(),
                component: "receiving".to_owned(),
                operation: "wamn-receiving:receiving/record-receipt@1.0.0".to_owned(),
                ..original.invocation.clone()
            }
        );

        target.scope.package_id = "wamn_inventory".to_owned();
        target.component = "inventory".to_owned();
        target.component_digest = "sha256:inventory".to_owned();
        let grandchild = child.retarget(&target, "wamn-inventory:inventory/receive@1.0.0");
        assert_eq!(grandchild.claims, original.claims);
        assert_eq!(grandchild.causation, original.causation);
        assert_eq!(
            grandchild.invocation,
            ConnectionInvocation {
                package_id: "wamn_inventory".to_owned(),
                component_digest: "sha256:inventory".to_owned(),
                component: "inventory".to_owned(),
                operation: "wamn-inventory:inventory/receive@1.0.0".to_owned(),
                ..original.invocation
            }
        );
    }

    #[test]
    fn shared_nested_import_retains_each_declaring_export() {
        let operation = "wamn-receiving:receiving/record-receipt@1.0.0";
        let digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let dependency = ComponentOperationDependency {
            package: "wamn_receiving".to_owned(),
            version: "1.0.0".to_owned(),
            digest: digest.to_owned(),
            operation: operation.to_owned(),
        };
        let target = AdmittedComponent {
            scope: ComponentPackageScope {
                tenant_id: "tenant-a".to_owned(),
                package_id: "wamn_receiving".to_owned(),
                package_version: "1.0.0".to_owned(),
            },
            component: "receiving".to_owned(),
            interface_version: "0.1.0".to_owned(),
            operations: BTreeMap::from([(
                operation.to_owned(),
                AdmittedComponentOperation {
                    registered_operation: Some(operation.to_owned()),
                    fresh_only: false,
                    committed_result_schema: None,
                    dependencies: Vec::new(),
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    statements: BTreeMap::new(),
                },
            )]),
            component_digest: digest.to_owned(),
            imports: Vec::new(),
            imports_fingerprint:
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            effects: Vec::<AdmittedComponentEffect>::new(),
        };

        let declaration = AdmittedComponentOperation {
            registered_operation: None,
            fresh_only: false,
            committed_result_schema: None,
            dependencies: vec![dependency],
            input_ports: Vec::new(),
            output_ports: Vec::new(),
            parameters: Vec::new(),
            statements: BTreeMap::new(),
        };
        let mut caller = target;
        caller.operations = BTreeMap::from([
            (
                "client-acme-receiving:one/run@3.0.0".to_owned(),
                declaration.clone(),
            ),
            (
                "client-acme-receiving:two/run@3.0.0".to_owned(),
                declaration,
            ),
        ]);
        let links = nested_operation_links(&caller).expect("matching pins may share one import");
        let (_, owners) = links
            .get(operation)
            .expect("the exact dependency import is present once");
        assert_eq!(links.len(), 1);
        assert_eq!(owners.len(), 2);
    }

    #[test]
    fn component_span_adopts_remote_traceparent_and_host_identity() {
        let harness = TraceHarness::install();
        let request = driver_request(Some(VALID_TRACEPARENT));
        let parent = remote_trace_context(&request).expect("valid W3C parent must extract");
        let span = component_invocation_span(
            &request,
            "project-a",
            7,
            "sha256:component",
            &node_call(),
            Some(&parent),
        );
        span.in_scope(|| {});
        drop(span);

        let spans = harness.spans();
        let component = span_named(&spans, "wamn.component.invoke");
        assert_eq!(component.span_context.trace_id().to_string(), TRACE_ID);
        assert_eq!(component.parent_span_id.to_string(), PARENT_SPAN_ID);
        assert_eq!(
            attribute(component, "wamn.tenant").as_deref(),
            Some("tenant-a")
        );
        assert_eq!(
            attribute(component, "wamn.project").as_deref(),
            Some("project-a")
        );
        assert_eq!(
            attribute(component, "wamn.environment").as_deref(),
            Some("prod")
        );
        assert_eq!(
            attribute(component, "wamn.wiring_id").as_deref(),
            Some("route-order")
        );
        assert_eq!(
            attribute(component, "wamn.wiring_version").as_deref(),
            Some("7")
        );
        assert_eq!(
            attribute(component, "wamn.component_digest").as_deref(),
            Some("sha256:component")
        );
        assert_eq!(
            attribute(component, "wamn.node_id").as_deref(),
            Some("load-order")
        );
        assert_eq!(
            attribute(component, "wamn.input_port").as_deref(),
            Some("request")
        );
        assert_eq!(attribute(component, "wamn.caller_principal_id"), None);
    }

    #[test]
    fn queue_component_span_inherits_the_host_created_root() {
        let harness = TraceHarness::install();
        let request = driver_request(None);
        let queue = tracing::info_span!(parent: None, "wamn.queue.delivery");
        let queue_context = queue.context();
        let queue_span_context = queue_context.span().span_context().clone();
        queue.in_scope(|| {
            let component = component_invocation_span(
                &request,
                "project-a",
                7,
                "sha256:component",
                &node_call(),
                None,
            );
            component.in_scope(|| {});
        });
        drop(queue);

        let spans = harness.spans();
        let root = span_named(&spans, "wamn.queue.delivery");
        let component = span_named(&spans, "wamn.component.invoke");
        assert_eq!(root.parent_span_id, opentelemetry::trace::SpanId::INVALID);
        assert_eq!(
            component.parent_span_id,
            queue_span_context.span_id(),
            "the queue invocation must remain a child of the executor root"
        );
        assert_eq!(
            component.span_context.trace_id(),
            root.span_context.trace_id()
        );
    }

    /// The guest must parent to the invocation the host is performing for it,
    /// not to whatever called the host. Forwarding `request.traceparent` made
    /// every downstream hop a sibling of `wamn.component.invoke`.
    #[test]
    fn node_context_carries_the_component_span_not_the_ingress_header() {
        let harness = TraceHarness::install();
        let request = driver_request(Some(VALID_TRACEPARENT));
        let parent = remote_trace_context(&request).expect("valid W3C parent must extract");
        let span = component_invocation_span(
            &request,
            "project-a",
            7,
            "sha256:component",
            &node_call(),
            Some(&parent),
        );
        let carried = span.in_scope(|| {
            node_context(&request, 7, &node_call(), 100).expect("node context must encode")
        });
        drop(span);

        let spans = harness.spans();
        let component = span_named(&spans, "wamn.component.invoke");
        assert_eq!(
            carried.traceparent.as_deref(),
            Some(
                format!(
                    "00-{}-{}-01",
                    component.span_context.trace_id(),
                    component.span_context.span_id()
                )
                .as_str()
            ),
            "the node must be handed the component span it is running under"
        );
        assert_ne!(
            carried.traceparent.as_deref(),
            Some(VALID_TRACEPARENT),
            "handing the raw ingress header on skips wamn.component.invoke"
        );
        assert_eq!(carried.tracestate.as_deref(), Some("vendor=value"));
    }

    /// Queued delivery carries no ingress header at all — the ratified
    /// host-scoped re-root — so injection is the ONLY thing that gives its guest
    /// a traceparent.
    #[test]
    fn queue_node_context_derives_a_traceparent_from_the_host_scoped_root() {
        let harness = TraceHarness::install();
        let request = driver_request(None);
        assert!(request.traceparent.is_none());
        let queue = tracing::info_span!(parent: None, "wamn.queue.delivery");
        let carried = queue.in_scope(|| {
            let component = component_invocation_span(
                &request,
                "project-a",
                7,
                "sha256:component",
                &node_call(),
                None,
            );
            let carried = component.in_scope(|| {
                node_context(&request, 7, &node_call(), 100).expect("node context must encode")
            });
            drop(component);
            carried
        });
        drop(queue);

        let spans = harness.spans();
        let root = span_named(&spans, "wamn.queue.delivery");
        let component = span_named(&spans, "wamn.component.invoke");
        assert_eq!(
            carried.traceparent.as_deref(),
            Some(
                format!(
                    "00-{}-{}-01",
                    root.span_context.trace_id(),
                    component.span_context.span_id()
                )
                .as_str()
            ),
            "the queued guest must join the executor's own root trace"
        );
        assert_eq!(carried.tracestate, None);
    }

    /// A subscriber without the OTel layer is the supported no-export mode: the
    /// span has no context to inject, and dropping the caller's header there
    /// would break W3C pass-through for a deployment that only forwards.
    #[test]
    fn without_an_otel_layer_the_ingress_header_passes_through() {
        let request = driver_request(Some(VALID_TRACEPARENT));
        let carried = tracing::info_span!("wamn.component.invoke").in_scope(|| {
            node_context(&request, 7, &node_call(), 100).expect("node context must encode")
        });
        assert_eq!(carried.traceparent.as_deref(), Some(VALID_TRACEPARENT));
        assert_eq!(carried.tracestate.as_deref(), Some("vendor=value"));
    }

    #[test]
    fn malformed_traceparent_is_ignored_without_suppressing_the_span() {
        let harness = TraceHarness::install();
        let request = driver_request(Some("not-a-traceparent"));
        assert!(remote_trace_context(&request).is_none());
        let span = component_invocation_span(
            &request,
            "project-a",
            7,
            "sha256:component",
            &node_call(),
            None,
        );
        span.in_scope(|| {});
        drop(span);

        let spans = harness.spans();
        let component = span_named(&spans, "wamn.component.invoke");
        assert_eq!(
            component.parent_span_id,
            opentelemetry::trace::SpanId::INVALID
        );
    }

    /// The un-fused plugin bind, driven through the production instantiation
    /// path: ORDER AND COUNT, per the ruling on wamn-0h0g.17.15.
    ///
    /// Linker entries land once per digest, before that digest's first
    /// registration; registration lands once per request. A skipped
    /// registration fails loud (a tenant-less scope has its postgres calls
    /// refused); entries re-added on every request under the cache would fail
    /// silently and only erode the win. This is the assertion that hears it.
    #[test]
    fn readiness_closure_contains_only_distinct_request_attachment_targets() {
        let manifest = ServingManifest {
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release: ServingRelease {
                tenant_id: "tenant-a".to_owned(),
                effective_release_id: EffectiveReleaseId::new(7).unwrap(),
                environment: "prod".to_owned(),
                packages: BTreeSet::from([PackageCoordinate::new("orders", "1.0.0").unwrap()]),
            },
            components: BTreeSet::new(),
            wirings: BTreeSet::new(),
            attachments: BTreeMap::from([
                (
                    "http".to_owned(),
                    attachment(AttachmentKind::Http, "request-wiring"),
                ),
                (
                    "internal".to_owned(),
                    attachment(AttachmentKind::Internal, "request-wiring"),
                ),
                (
                    "studio".to_owned(),
                    attachment(AttachmentKind::Studio, "studio-wiring"),
                ),
                (
                    "cron".to_owned(),
                    attachment(AttachmentKind::Cron, "background-wiring"),
                ),
            ]),
            registrations: BTreeMap::from([(
                "orders::events".to_owned(),
                ServingRegistration {
                    package_id: "orders".to_owned(),
                    source_package_id: "orders".to_owned(),
                    wiring_id: "stream-wiring".to_owned(),
                    wiring_version: 4,
                    entity: "order".to_owned(),
                    ops: BTreeSet::from(["created".to_owned()]),
                    input: ServingRegistrationInput::Event,
                },
            )]),
        };

        assert_eq!(
            synchronous_wiring_targets(&manifest),
            BTreeSet::from([
                ("orders".to_owned(), "request-wiring".to_owned(), 3),
                ("orders".to_owned(), "studio-wiring".to_owned(), 3),
            ])
        );
    }
}

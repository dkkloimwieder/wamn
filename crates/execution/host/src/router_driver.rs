//! The single production driver for direct and queued wiring delivery.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use futures_util::{StreamExt as _, stream};
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::TraceContextExt as _;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use wamn_catalog::{
    AdmittedComponent, ArtifactHash, AttachmentKind, ComponentOperationDependency, DefinitionHash,
    ServingComponent, ServingComponentOperation, ServingManifest, ServingWiring,
};
use wamn_control_registry::identifiers::valid_runner;
use wamn_router::{
    ActiveWiring, CacheInsert, Delivery, ErrorDetail, Lookup, NodeError, NodeOutcome, Outcome,
    RateLimitDetail, Step, WiringCache, WiringCacheSnapshot,
};
use wamn_runtime::component_artifact_source::{
    ComponentArtifactFetchErrorKind, ComponentArtifactSource,
};
use wamn_runtime::engine::MAX_HOST_CALL_DURATION;
use wamn_runtime::plugins::connection_http::{
    self, CONNECTION_HTTP_ID, ConnectionExecutionClosure, ConnectionHttp, ConnectionInvocation,
};
use wamn_runtime::plugins::flow_http_routing::AuthenticatedCaller;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::{WAMN_LOGGING_ID, WamnLogging};
use wamn_runtime::plugins::wamn_postgres::{
    CandidateBindingWorld, CandidateWiringResolution, ReleaseIdentity, ResolvedActiveWiring,
    SessionClaims, WAMN_POSTGRES_ID, WamnPostgres,
};
use wamn_runtime::release_manifest::ReleaseManifestWeld;
use wamn_runtime::wiring_doorbell::WiringDoorbellListener;
use wash_runtime::engine::Engine;
use wash_runtime::engine::InstancePolicy;
use wash_runtime::engine::ctx::{Ctx, SharedCtx};
use wash_runtime::engine::workload::{WorkloadComponent, WorkloadItem};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component, Instance, Linker, TypedFunc};

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

/// Cadence of the epoch ticker owned by wash-runtime v2.8.
///
/// Manual stores set deadlines in ticks, while wash-runtime keeps its ticker
/// private. Keep this conversion beside the only manual store construction
/// site and revalidate it on every runtime sync.
///
/// THE UPSTREAM HALF IS `EPOCH_TICK` in wash-runtime's `src/engine/mod.rs`,
/// declared `pub(crate)` — it cannot be imported, only restated here, so this
/// is a mirror and not a reference. A cadence change upstream that is not
/// mirrored here rescales EVERY node deadline silently, because both halves
/// keep compiling and deadlines still fire, just at the wrong wall time. Both
/// literals are pinned together by
/// `tests/conformance/src/runtime_inventory.rs::the_manual_store_epoch_tick_still_mirrors_the_runtime_ticker`
/// (`wamn-k9ea`), which is what makes the next sync notice.
const MANUAL_STORE_EPOCH_TICK: Duration = Duration::from_millis(10);

/// Cold-preload compiler workers per serving process.
///
/// Both shipped serving groups have two CPU cores. Two workers use that
/// capacity without making six large Cranelift compilations or six live Stores
/// contend at once. Instantiation remains serial below, preserving the
/// readiness proof's original one-component resource grain.
const PRELOAD_COMPILATION_CONCURRENCY: usize = 2;

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

/// Exact operation authority missing from the originating caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PermissionDenied {
    operation: Box<str>,
}

impl PermissionDenied {
    pub(crate) fn new(operation: impl Into<Box<str>>) -> Self {
        Self {
            operation: operation.into(),
        }
    }

    pub(crate) fn operation(&self) -> &str {
        &self.operation
    }
}

impl fmt::Display for PermissionDenied {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "permission denied for operation {}",
            self.operation
        )
    }
}

impl std::error::Error for PermissionDenied {}

pub(crate) fn authorize_registered_operation(
    caller: Option<&AuthenticatedCaller>,
    operation: Option<&str>,
) -> Result<(), PermissionDenied> {
    let Some(operation) = operation else {
        return Ok(());
    };
    if caller.is_some_and(|caller| caller.permits(operation)) {
        Ok(())
    } else {
        Err(PermissionDenied::new(operation))
    }
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
/// `opentelemetry::global::tracer`: the fork's `initialize_observability`
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
        wamn.wiring_id = %request.wiring_id,
        wamn.wiring_version = wiring_version,
        wamn.component_digest = %component_digest,
        wamn.node_id = %call.node,
        wamn.operation = %call.operation,
        wamn.caller_principal_id = tracing::field::Empty,
        wamn.input_port = tracing::field::Empty,
    );
    if let Some(caller) = request.caller.as_ref() {
        span.record("wamn.caller_principal_id", caller.principal_id());
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
}

impl CatalogFacts {
    fn from_resolved(resolved: &ResolvedActiveWiring) -> Self {
        Self {
            effective_release_id: resolved.effective_release_id,
            components: Arc::clone(&resolved.components),
        }
    }

    fn component(&self, digest: &str) -> Option<&AdmittedComponent> {
        self.components
            .iter()
            .find(|component| component.component_digest == digest)
    }
}

#[derive(Debug, Clone, Copy)]
enum ExecutionClosure<'a> {
    Released,
    Candidate {
        target: &'a CandidateWiringTarget,
        binding_world: &'a Arc<CandidateBindingWorld>,
        component_bytes: &'a BTreeMap<String, Vec<u8>>,
    },
}

/// One router, cache, and artifact source per serving process. Both process
/// leaves construct this exact type.
pub struct RouterDriver {
    engine: Arc<Engine>,
    postgres: Arc<WamnPostgres>,
    credentials: Arc<WamnCredentials>,
    logging: Arc<WamnLogging>,
    allowed_hosts: Arc<[AllowedHost]>,
    release: Arc<ReleaseManifestWeld>,
    source: ComponentArtifactSource,
    config: RouterDriverConfig,
    cache: Arc<WiringCache<CatalogFacts>>,
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
    #[expect(
        clippy::too_many_arguments,
        reason = "each host-owned capability is an independent production dependency"
    )]
    pub fn new(
        engine: Arc<Engine>,
        postgres: Arc<WamnPostgres>,
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
            credentials,
            logging,
            allowed_hosts,
            release,
            source,
            config,
            cache,
            _doorbell: doorbell,
            started: Instant::now(),
        })
    }

    pub fn snapshot(&self) -> RouterDriverSnapshot {
        RouterDriverSnapshot {
            wiring_cache: self.cache.snapshot(),
        }
    }

    /// Prepare the exact release closure reachable from synchronous request
    /// attachments.
    ///
    /// HTTP, internal and studio attachments participate. Cron attachments and
    /// registrations are background delivery and therefore do not enlarge the
    /// request readiness set. Every selected wiring is resolved through this
    /// driver's one cache, every admitted component tuple is checked against the
    /// welded manifest, all of its exact environment bindings are proven, and
    /// one clean instance per digest is instantiated and dropped to prove the
    /// closure is servable. No node handler is invoked.
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
        let mut components = BTreeMap::<String, AdmittedComponent>::new();
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
            let resolved = self.resolve_frozen(&request).await.with_context(|| {
                format!("preload release wiring {wiring_id:?} version {wiring_version}")
            })?;
            self.validate_wiring_closure(&request, &resolved)?;
            for component in resolved.facts.components.iter() {
                self.validate_release_component(component)?;
                if let Some(existing) =
                    components.insert(component.component_digest.clone(), component.clone())
                {
                    anyhow::ensure!(
                        existing == *component,
                        "release-component-digest-fact-mismatch"
                    );
                }
            }
        }

        let component_digests = components.keys().cloned().collect::<Vec<_>>();
        let bindings_ready = self
            .postgres
            .release_component_bindings_ready(
                &self.config.project,
                &manifest.release.tenant_id,
                manifest.release.effective_release_id.get(),
                &manifest.release.environment,
                &component_digests,
            )
            .await
            .context("verify synchronous release connection bindings")?;
        anyhow::ensure!(bindings_ready, "release-component-requirement-unbound");

        let components: Arc<[AdmittedComponent]> = components.into_values().collect();
        let component_count = components.len();
        let pipelines = components.iter().cloned().map(|component| {
            let source = self.source.clone();
            let engine = Arc::clone(&self.engine);
            async move {
                let component_started = Instant::now();
                let pull_started = Instant::now();
                let bytes = source.pull_verified(&component).await.with_context(|| {
                    format!(
                        "preload release component digest {:?}",
                        component.component_digest
                    )
                })?;
                let component_bytes = bytes.len();
                tracing::info!(
                    target: "wamn::router",
                    component_digest = %component.component_digest,
                    component_bytes,
                    elapsed_ms = %pull_started.elapsed().as_millis(),
                    "release component pull completed"
                );

                let compile_wall_started = Instant::now();
                let compile_engine = Arc::clone(&engine);
                let (compiled, compile_elapsed) = tokio::task::spawn_blocking(move || {
                    let compile_started = Instant::now();
                    (
                        NodeInstance::compile(&compile_engine, &bytes),
                        compile_started.elapsed(),
                    )
                })
                .await
                .context("join release component compilation task")?;
                let compiled = compiled.with_context(|| {
                    format!(
                        "compile release component digest {:?}",
                        component.component_digest
                    )
                })?;
                tracing::info!(
                    target: "wamn::router",
                    component_digest = %component.component_digest,
                    compile_ms = %compile_elapsed.as_millis(),
                    compile_wall_ms = %compile_wall_started.elapsed().as_millis(),
                    "release component compilation completed"
                );
                anyhow::Ok((component, compiled, component_started))
            }
        });
        let mut pipelines = stream::iter(pipelines).buffered(PRELOAD_COMPILATION_CONCURRENCY);
        while let Some(result) = pipelines.next().await {
            let (component, compiled, component_started) = result?;
            let instantiate_started = Instant::now();
            NodeInstance::instantiate_compiled(
                &self.engine,
                compiled,
                Arc::clone(&self.postgres),
                Arc::clone(&self.credentials),
                Arc::clone(&self.logging),
                Arc::clone(&self.allowed_hosts),
                Arc::clone(&self.release),
                self.source.clone(),
                Arc::clone(&components),
                &self.config,
                &manifest.release.tenant_id,
                &component,
            )
            .await
            .with_context(|| {
                format!(
                    "pre-instantiate release component digest {:?}",
                    component.component_digest
                )
            })?;
            tracing::info!(
                target: "wamn::router",
                component_digest = %component.component_digest,
                elapsed_ms = %instantiate_started.elapsed().as_millis(),
                total_elapsed_ms = %component_started.elapsed().as_millis(),
                "release component instantiation completed"
            );
        }
        tracing::info!(
            target: "wamn::router",
            synchronous_wirings = targets.len(),
            component_digests = component_count,
            elapsed_ms = %prepare_started.elapsed().as_millis(),
            "synchronous release preload completed"
        );
        Ok(PreparedReleaseReadiness {
            synchronous_wirings: targets.len(),
            component_digests: component_count,
        })
    }

    /// Execute one direct or queued delivery through the same router and node
    /// invoker. The caller owns acting on the terminal verdict.
    pub async fn execute(&self, request: RouterDriverRequest) -> anyhow::Result<RouterDelivery> {
        self.validate_request_scope(&request)?;
        let active = self
            .resolve(&request)
            .instrument(tracing::info_span!("wamn.router.resolve"))
            .await?;
        self.validate_wiring_closure(&request, &active)?;
        self.execute_resolved(request, active, ExecutionClosure::Released)
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
        self.execute_resolved(
            driver_request,
            active,
            ExecutionClosure::Candidate {
                target,
                binding_world: &request.binding_world,
                component_bytes: &component_bytes,
            },
        )
        .await
    }

    async fn execute_resolved(
        &self,
        request: RouterDriverRequest,
        active: ActiveWiring<CatalogFacts>,
        closure: ExecutionClosure<'_>,
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
        loop {
            let now_ms = self.now_ms();
            match wiring.next(&mut walk, now_ms) {
                Step::Done(status) => {
                    return Ok(RouterDelivery {
                        wiring_version: active.version,
                        graph_hash: Arc::clone(&active.graph_hash),
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
                    let component = active
                        .facts
                        .component(&call.component)
                        .ok_or_else(|| anyhow::anyhow!("router-node-component-fact-missing"))?;
                    let operation = component
                        .operation(&call.operation)
                        .ok_or_else(|| anyhow::anyhow!("router-node-operation-fact-missing"))?;
                    authorize_registered_operation(
                        request.caller.as_ref(),
                        operation.registered_operation.as_deref(),
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
                        .invoke_node(&request, &active, &call, closure)
                        .instrument(span)
                        .await
                        .with_context(|| format!("invoke wiring node {:?}", call.node))?;
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
        let facts = CatalogFacts::from_resolved(&resolved);
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
            let facts = CatalogFacts::from_resolved(&resolved);
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
        let facts = CatalogFacts::from_resolved(&resolved);
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

    async fn invoke_node(
        &self,
        request: &RouterDriverRequest,
        active: &ActiveWiring<CatalogFacts>,
        call: &wamn_router::NodeCall,
        closure: ExecutionClosure<'_>,
    ) -> anyhow::Result<NodeOutcome> {
        let component = active
            .facts
            .component(&call.component)
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
                package_id: component.scope.package_id.clone(),
                wiring_id: request.wiring_id.clone(),
                wiring_version: active.version,
                node_id: call.node.clone(),
                occurrence: call.occurrence,
                component_digest: component.component_digest.clone(),
                closure: connection_closure,
            },
        };
        let candidate_bytes = match closure {
            ExecutionClosure::Released => None,
            ExecutionClosure::Candidate {
                component_bytes, ..
            } => Some(
                component_bytes
                    .get(&component.component_digest)
                    .ok_or_else(|| {
                        CandidateExecutionRefusal::new(
                            CandidateExecutionRefusalKind::Artifact,
                            "candidate-component-bytes-missing",
                        )
                    })?,
            ),
        };
        let pulled_bytes;
        let bytes = match candidate_bytes {
            Some(bytes) => bytes.as_slice(),
            None => {
                pulled_bytes = self
                    .source
                    .pull_verified(component)
                    .instrument(tracing::info_span!("wamn.component.pull"))
                    .await?;
                pulled_bytes.as_slice()
            }
        };
        let mut instance = NodeInstance::instantiate(
            &self.engine,
            bytes,
            Arc::clone(&self.postgres),
            Arc::clone(&self.credentials),
            Arc::clone(&self.logging),
            Arc::clone(&self.allowed_hosts),
            Arc::clone(&self.release),
            self.source.clone(),
            Arc::clone(&active.facts.components),
            &self.config,
            &request.tenant_id,
            component,
        )
        .await?;
        instance
            .bind_identity(&acquisition, request.caller.as_ref())
            .with_context(|| format!("bind identity to {} instance", component.component_digest))?;
        let deadline_ms = bounded_node_deadline_ms(call.deadline_ms);
        let context = node_context(request, active.version, call, deadline_ms)?;
        let input = serde_json::to_string(&call.payload).context("encode node input")?;
        // The instance is destroyed at the end of this invocation either way;
        // its `Drop` clears the identity it was bound to before it goes.
        instance
            .run(&call.operation, &context, &input, deadline_ms)
            .await
            .and_then(lower_node_outcome)
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
                        dependencies: operation.dependencies.clone(),
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
}

#[derive(Debug, Clone)]
struct BoundNestedInvocation {
    caller: Option<AuthenticatedCaller>,
    acquisition: NodeAcquisition,
    active_operation: Option<Box<str>>,
}

/// The exact released closure and originating authority available to imports.
///
/// Linker construction installs no identity. [`NodeInstance::bind_identity`]
/// populates the slot only after every host capability is bound, and
/// [`NodeInstance::run`] names the one export currently allowed to use its
/// admission-declared dependencies.
struct NestedOperationHost {
    engine: Arc<Engine>,
    postgres: Arc<WamnPostgres>,
    credentials: Arc<WamnCredentials>,
    logging: Arc<WamnLogging>,
    allowed_hosts: Arc<[AllowedHost]>,
    release: Arc<ReleaseManifestWeld>,
    source: ComponentArtifactSource,
    config: RouterDriverConfig,
    tenant_id: Box<str>,
    components: Arc<[AdmittedComponent]>,
    invocation: std::sync::Mutex<Option<BoundNestedInvocation>>,
}

impl fmt::Debug for NestedOperationHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NestedOperationHost")
            .field("tenant_id", &self.tenant_id)
            .field("components", &self.components.len())
            .finish_non_exhaustive()
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
    if let Some(denial) = error.downcast_ref::<PermissionDenied>() {
        return wash_runtime::wasmtime::Error::new(denial.clone());
    }
    if let Some(refusal) = error.downcast_ref::<NestedOperationRefusal>() {
        return wash_runtime::wasmtime::Error::new(refusal.clone());
    }
    wash_runtime::wasmtime::Error::msg(format!("{error:#}"))
}

struct ActiveNestedOperation {
    host: Arc<NestedOperationHost>,
}

impl Drop for ActiveNestedOperation {
    fn drop(&mut self) {
        if let Some(invocation) = self
            .host
            .invocation
            .lock()
            .expect("nested invocation lock must not be poisoned")
            .as_mut()
        {
            invocation.active_operation = None;
        }
    }
}

impl NestedOperationHost {
    fn bind(&self, acquisition: &NodeAcquisition, caller: Option<&AuthenticatedCaller>) {
        *self
            .invocation
            .lock()
            .expect("nested invocation lock must not be poisoned") = Some(BoundNestedInvocation {
            caller: caller.cloned(),
            acquisition: acquisition.clone(),
            active_operation: None,
        });
    }

    fn revoke(&self) {
        *self
            .invocation
            .lock()
            .expect("nested invocation lock must not be poisoned") = None;
    }

    fn activate(self: &Arc<Self>, operation: &str) -> anyhow::Result<ActiveNestedOperation> {
        let mut invocation = self
            .invocation
            .lock()
            .expect("nested invocation lock must not be poisoned");
        let invocation = invocation.as_mut().ok_or_else(|| {
            NestedOperationRefusal::new(NestedOperationRefusalKind::IdentityUnbound, operation)
        })?;
        invocation.active_operation = Some(operation.into());
        Ok(ActiveNestedOperation {
            host: Arc::clone(self),
        })
    }

    fn bound_for(
        &self,
        owner_operations: &BTreeSet<String>,
        dependency: &ComponentOperationDependency,
    ) -> Result<BoundNestedInvocation, NestedOperationRefusal> {
        let invocation = self
            .invocation
            .lock()
            .expect("nested invocation lock must not be poisoned")
            .clone()
            .ok_or_else(|| {
                NestedOperationRefusal::new(
                    NestedOperationRefusalKind::IdentityUnbound,
                    &dependency.operation,
                )
            })?;
        if !invocation
            .active_operation
            .as_deref()
            .is_some_and(|operation| owner_operations.contains(operation))
        {
            return Err(NestedOperationRefusal::new(
                NestedOperationRefusalKind::UndeclaredForExport,
                &dependency.operation,
            ));
        }
        if invocation.acquisition.claims.release.is_none() {
            return Err(NestedOperationRefusal::new(
                NestedOperationRefusalKind::ReleaseClosureUnavailable,
                &dependency.operation,
            ));
        }
        Ok(invocation)
    }

    fn resolve_target(
        &self,
        dependency: &ComponentOperationDependency,
    ) -> Result<AdmittedComponent, NestedOperationRefusal> {
        resolve_nested_target(&self.tenant_id, &self.components, dependency)
            .cloned()
            .ok_or_else(|| {
                NestedOperationRefusal::new(
                    NestedOperationRefusalKind::ReleaseClosureUnavailable,
                    &dependency.operation,
                )
            })
    }

    async fn invoke(
        self: Arc<Self>,
        owner_operations: Arc<BTreeSet<String>>,
        dependency: ComponentOperationDependency,
        context: node_types::NodeContext,
        input: String,
    ) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
        let bound = self.bound_for(&owner_operations, &dependency)?;
        let target = self.resolve_target(&dependency)?;
        let target_operation = target
            .operation(&dependency.operation)
            .expect("resolve_target proves the exact operation exists");
        authorize_registered_operation(
            bound.caller.as_ref(),
            target_operation.registered_operation.as_deref(),
        )?;
        validate_component_in_release(&self.release, &target)?;

        let bytes = self
            .source
            .pull_verified(&target)
            .instrument(tracing::info_span!(
                "wamn.component.pull",
                wamn.operation = %dependency.operation,
                wamn.component_digest = %dependency.digest,
            ))
            .await?;
        let mut child = NodeInstance::instantiate(
            &self.engine,
            &bytes,
            Arc::clone(&self.postgres),
            Arc::clone(&self.credentials),
            Arc::clone(&self.logging),
            Arc::clone(&self.allowed_hosts),
            Arc::clone(&self.release),
            self.source.clone(),
            Arc::clone(&self.components),
            &self.config,
            &self.tenant_id,
            &target,
        )
        .await?;
        let mut acquisition = bound.acquisition;
        acquisition.invocation.package_id = target.scope.package_id.clone();
        acquisition.invocation.component_digest = target.component_digest.clone();
        child.bind_identity(&acquisition, bound.caller.as_ref())?;
        let span = tracing::info_span!(
            "wamn.component.invoke",
            wamn.tenant = %self.tenant_id,
            wamn.project = %self.config.project,
            wamn.wiring_id = %context.wiring_id,
            wamn.wiring_version = context.wiring_version,
            wamn.component_digest = %dependency.digest,
            wamn.node_id = %context.node_id,
            wamn.operation = %dependency.operation,
            wamn.caller_principal_id = tracing::field::Empty,
        );
        if let Some(caller) = bound.caller.as_ref() {
            span.record("wamn.caller_principal_id", caller.principal_id());
        }
        child
            .run(
                &dependency.operation,
                &context,
                &input,
                bounded_node_deadline_ms(context.deadline_ms),
            )
            .instrument(span)
            .await
    }
}

fn resolve_nested_target<'a>(
    tenant_id: &str,
    components: &'a [AdmittedComponent],
    dependency: &ComponentOperationDependency,
) -> Option<&'a AdmittedComponent> {
    components.iter().find(|component| {
        component.scope.tenant_id == tenant_id
            && component.scope.package_id == dependency.package
            && component.scope.package_version == dependency.version
            && component.component_digest == dependency.digest
            && component.operation(&dependency.operation).is_some()
    })
}

fn add_nested_operation_links(
    linker: &mut Linker<SharedCtx>,
    host: Arc<NestedOperationHost>,
    component: &AdmittedComponent,
) -> anyhow::Result<()> {
    for (_, (dependency, owner_operations)) in nested_operation_links(component)? {
        let owner_operations = Arc::new(owner_operations);
        let nested = Arc::clone(&host);
        linker.instance(&dependency.operation)?.func_wrap_async(
            "run",
            move |_store, (context, input): (node_types::NodeContext, String)| {
                let nested = Arc::clone(&nested);
                let owner_operations = Arc::clone(&owner_operations);
                let dependency = dependency.clone();
                Box::new(async move {
                    let result = nested
                        .invoke(owner_operations, dependency, context, input)
                        .await
                        .map_err(nested_host_error)?;
                    Ok((result,))
                })
            },
        )?;
    }
    Ok(())
}

fn nested_operation_links(
    component: &AdmittedComponent,
) -> anyhow::Result<BTreeMap<String, (ComponentOperationDependency, BTreeSet<String>)>> {
    let mut links = BTreeMap::<String, (ComponentOperationDependency, BTreeSet<String>)>::new();
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

struct NodeInstance {
    store: Store<SharedCtx>,
    node: Instance,
    postgres: Arc<WamnPostgres>,
    logging: Arc<WamnLogging>,
    connection_http: Arc<ConnectionHttp>,
    nested: Arc<NestedOperationHost>,
    scope: Box<str>,
}

impl fmt::Debug for NodeInstance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NodeInstance")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl NodeInstance {
    fn compile(engine: &Engine, bytes: &[u8]) -> anyhow::Result<Component> {
        Component::new(engine.inner(), bytes)
            .map_err(|error| anyhow::anyhow!("compile wamn:node: {error}"))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "instance construction welds each independent host capability"
    )]
    async fn instantiate(
        engine: &Engine,
        bytes: &[u8],
        postgres: Arc<WamnPostgres>,
        credentials: Arc<WamnCredentials>,
        logging: Arc<WamnLogging>,
        allowed_hosts: Arc<[AllowedHost]>,
        release: Arc<ReleaseManifestWeld>,
        source: ComponentArtifactSource,
        components: Arc<[AdmittedComponent]>,
        config: &RouterDriverConfig,
        tenant_id: &str,
        component_fact: &AdmittedComponent,
    ) -> anyhow::Result<Self> {
        let component = tracing::info_span!("wamn.component.compile")
            .in_scope(|| Self::compile(engine, bytes))?;
        Self::instantiate_compiled(
            engine,
            component,
            postgres,
            credentials,
            logging,
            allowed_hosts,
            release,
            source,
            components,
            config,
            tenant_id,
            component_fact,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "instance construction welds each independent host capability"
    )]
    async fn instantiate_compiled(
        engine: &Engine,
        component: Component,
        postgres: Arc<WamnPostgres>,
        credentials: Arc<WamnCredentials>,
        logging: Arc<WamnLogging>,
        allowed_hosts: Arc<[AllowedHost]>,
        release: Arc<ReleaseManifestWeld>,
        source: ComponentArtifactSource,
        components: Arc<[AdmittedComponent]>,
        config: &RouterDriverConfig,
        tenant_id: &str,
        component_fact: &AdmittedComponent,
    ) -> anyhow::Result<Self> {
        let (mut store, mut workload, connection_http, nested, scope) = async {
            let mut linker: Linker<SharedCtx> = Linker::new(engine.inner());
            wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
            let local = wash_runtime::types::LocalResources {
                allowed_hosts: Arc::clone(&allowed_hosts),
                ..Default::default()
            };
            let connection_http = Arc::new(ConnectionHttp::new(
                Arc::clone(&postgres),
                Arc::clone(&credentials),
                tenant_id,
                config.project.as_str(),
                Arc::clone(&allowed_hosts),
                Some(Arc::clone(&release)),
            ));
            let nested = Arc::new(NestedOperationHost {
                engine: Arc::new(engine.clone()),
                postgres: Arc::clone(&postgres),
                credentials: Arc::clone(&credentials),
                logging: Arc::clone(&logging),
                allowed_hosts: Arc::clone(&allowed_hosts),
                release: Arc::clone(&release),
                source,
                config: config.clone(),
                tenant_id: tenant_id.into(),
                components,
                invocation: std::sync::Mutex::new(None),
            });
            add_nested_operation_links(&mut linker, Arc::clone(&nested), component_fact)?;
            let loopback = Arc::new(std::sync::Mutex::new(
                wash_runtime::sockets::loopback::Network::default(),
            ));
            let mut workload = WorkloadComponent::new(
                "router-driver",
                "router-driver",
                "wamn",
                component_fact.component.as_str(),
                component,
                linker,
                Vec::new(),
                local,
                loopback,
                InstancePolicy::Ephemeral,
            );
            let imports = workload.world().imports;
            // The driver has one credential-exact project. Route every named
            // Postgres instance through that same trusted project rather than
            // bypassing the plugin's `(implements ...)` binder with a raw linker.
            let imports: HashSet<_> = imports
                .into_iter()
                .map(|mut interface| {
                    if interface.namespace == "wamn"
                        && interface.package == "postgres"
                        && interface.name.is_some()
                    {
                        interface
                            .config
                            .insert("project".to_owned(), config.project.clone());
                    }
                    interface
                })
                .collect();
            {
                let mut item = WorkloadItem::Component(&mut workload);
                postgres
                    .on_workload_item_bind(&mut item, WitInterfaces::new(&imports))
                    .await?;
                logging
                    .on_workload_item_bind(&mut item, WitInterfaces::new(&imports))
                    .await?;
                if WitInterfaces::new(&imports).contains("wamn", "connection", &["http"]) {
                    connection_http::add_to_linker(item.linker())?;
                }
            }
            let scope: Box<str> = workload.id().into();
            // Linker setup is not an identity bind. In particular WamnLogging's
            // plugin hook seeds even an empty claim. Clear every registry before
            // component instantiation so start functions cannot exercise tenant
            // authority; `bind_identity` is the sole identity installation point.
            postgres.revoke_session_claims(&scope);
            logging.clear_claim(&scope);
            connection_http.revoke_invocation(&scope);
            nested.revoke();
            let mut plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> =
                HashMap::new();
            plugins.insert(WAMN_POSTGRES_ID, Arc::clone(&postgres) as _);
            plugins.insert(WAMN_LOGGING_ID, Arc::clone(&logging) as _);
            plugins.insert(CONNECTION_HTTP_ID, Arc::clone(&connection_http) as _);
            let ctx = Ctx::builder(scope.to_string(), scope.to_string())
                .with_plugins(plugins)
                .build();
            let mut store = Store::new(engine.inner(), SharedCtx::new(ctx));
            // Instantiation executes guest start code, so it needs the same bounded
            // ceiling as a call. One tick is only 10 ms and interrupts valid
            // virtualized std components before their instance is ready.
            store.set_epoch_deadline(deadline_ticks(bounded_node_deadline_ms(None)));
            Ok::<_, anyhow::Error>((store, workload, connection_http, nested, scope))
        }
        .instrument(tracing::info_span!("wamn.component.linker_setup"))
        .await?;
        let compiled = workload.component().clone();
        let pre = tracing::info_span!("wamn.component.link")
            .in_scope(|| workload.linker().instantiate_pre(&compiled))?;
        let node = pre
            .instantiate_async(&mut store)
            .instrument(tracing::info_span!("wamn.component.instantiate"))
            .await
            .map_err(|error| anyhow::anyhow!("instantiate wamn:node: {error}"))?;
        Ok(Self {
            store,
            node,
            postgres,
            logging,
            connection_http,
            nested,
            scope,
        })
    }

    async fn run(
        &mut self,
        operation: &str,
        context: &node_types::NodeContext,
        input: &String,
        deadline_ms: u64,
    ) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
        self.store.set_epoch_deadline(deadline_ticks(deadline_ms));
        let _active_operation = self.nested.activate(operation)?;
        let handler = self
            .node
            .get_export_index(&mut self.store, None, operation)
            .ok_or_else(|| anyhow::anyhow!("component has no exported operation {operation:?}"))?;
        let run = self
            .node
            .get_export_index(&mut self.store, Some(&handler), "run")
            .ok_or_else(|| anyhow::anyhow!("operation {operation:?} has no handler.run export"))?;
        let run: TypedFunc<
            (&node_types::NodeContext, &str),
            (Result<node_types::Emission, node_types::NodeError>,),
        > = self
            .node
            .get_typed_func(&mut self.store, &run)
            .map_err(|error| {
                anyhow::anyhow!("operation {operation:?} handler.run has wrong type: {error}")
            })?;
        let (outcome,) = match run
            .call_async(&mut self.store, (context, input.as_str()))
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                let error: anyhow::Error = error.into();
                return Err(error.context(format!("operation {operation:?} handler.run trapped")));
            }
        };
        Ok(outcome)
    }

    /// Bind EVERY identity-derived registry entry of this instance.
    ///
    /// Called once, before the guest runs, so no instance is ever invoked under
    /// an identity other than the one acquiring it. An element that is
    /// identity-derived and not bound here is a cross-tenant leak channel; add
    /// it to [`revoke_identity`](Self::revoke_identity) in the same change.
    fn bind_identity(
        &mut self,
        identity: &NodeAcquisition,
        caller: Option<&AuthenticatedCaller>,
    ) -> anyhow::Result<()> {
        self.postgres
            .bind_session_claims(&self.scope, &identity.claims)?;
        self.logging.set_claim(
            &self.scope,
            &identity.claims.tenant,
            identity
                .claims
                .project
                .as_deref()
                .unwrap_or(wamn_runtime::plugins::wamn_postgres::DEFAULT_PROJECT),
        );
        if let Err(error) = self
            .connection_http
            .bind_invocation(&self.scope, identity.invocation.clone())
        {
            self.logging.clear_claim(&self.scope);
            self.postgres.revoke_session_claims(&self.scope);
            return Err(error);
        }
        self.nested.bind(identity, caller);
        Ok(())
    }

    /// Clear every element [`bind_identity`](Self::bind_identity) installed.
    ///
    /// Each call is a scope-keyed removal that `instantiate` already makes with
    /// nothing bound, so this is safe on an unbound instance and runs from
    /// `Drop` on every path that ends an invocation, cancellation included.
    fn revoke_identity(&mut self) {
        self.nested.revoke();
        self.connection_http.revoke_invocation(&self.scope);
        self.logging.clear_claim(&self.scope);
        self.postgres.revoke_session_claims(&self.scope);
    }
}

impl Drop for NodeInstance {
    fn drop(&mut self) {
        self.revoke_identity();
    }
}

fn deadline_ticks(deadline_ms: u64) -> u64 {
    let ticks = Duration::from_millis(deadline_ms)
        .as_nanos()
        .div_ceil(MANUAL_STORE_EPOCH_TICK.as_nanos());
    u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
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
pub(crate) async fn real_nested_permission_denial(
    operation: &'static str,
) -> anyhow::Result<anyhow::Error> {
    const CALLER_OPERATION: &str = "client-acme-receiving:receiving/record-receipt@3.0.0";
    let bytes = wat::parse_str(format!(
        r#"(component
          (import "{operation}" (instance $dependency
            (export "run" (func))
          ))
          (core func $dependency-run (canon lower (func $dependency "run")))
          (core module $wrapper
            (import "dependency" "run" (func $run))
            (func (export "run")
              call $run
            )
          )
          (core instance $imports
            (export "run" (func $dependency-run))
          )
          (core instance $wrapped (instantiate $wrapper
            (with "dependency" (instance $imports))
          ))
          (func $run (canon lift (core func $wrapped "run")))
          (instance $caller
            (export "run" (func $run))
          )
          (export "{CALLER_OPERATION}" (instance $caller))
        )"#
    ))
    .context("encode nested permission-denial component fixture")?;
    let engine = wash_runtime::wasmtime::Engine::default();
    let component = Component::new(&engine, bytes)?;
    let mut linker = Linker::<()>::new(&engine);
    linker
        .instance(operation)?
        .func_wrap_async::<(), (), _>("run", move |_store, ()| {
            Box::new(async move {
                Err(wash_runtime::wasmtime::Error::new(PermissionDenied::new(
                    operation,
                )))
            })
        })?;
    let mut store = wash_runtime::wasmtime::Store::new(&engine, ());
    let instance = linker.instantiate_async(&mut store, &component).await?;
    let handler = instance
        .get_export_index(&mut store, None, CALLER_OPERATION)
        .context("fixture must export the caller operation")?;
    let run = instance
        .get_export_index(&mut store, Some(&handler), "run")
        .context("fixture caller operation must export run")?;
    let run: wash_runtime::wasmtime::component::TypedFunc<(), ()> =
        instance.get_typed_func(&mut store, &run)?;
    let error = run
        .call_async(&mut store, ())
        .await
        .expect_err("the real nested host import must deny the operation");
    let error: anyhow::Error = error.into();
    Ok(error
        .context("operation handler.run trapped")
        .context("invoke wiring node"))
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
        assert_eq!(deadline_ticks(30), 3);
        assert_eq!(deadline_ticks(1), 1);
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

        assert!(authorize_registered_operation(None, None).is_ok());
        let denial = authorize_registered_operation(None, Some(operation))
            .expect_err("a registered invocation without an originating caller is denied");
        assert_eq!(denial.operation(), operation);
    }

    #[tokio::test]
    async fn nested_permission_denial_survives_the_real_component_boundary() {
        let operation = "wamn-receiving:receiving/record-receipt@1.0.0";
        let error = real_nested_permission_denial(operation)
            .await
            .expect("the component fixture must execute");

        let denial = error
            .downcast_ref::<PermissionDenied>()
            .expect("the router-delivery boundary must see the original denial type");
        assert_eq!(denial.operation(), operation);
    }

    #[test]
    fn nested_target_resolution_requires_the_exact_released_coordinate() {
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
                    dependencies: Vec::new(),
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                },
            )]),
            component_digest: digest.to_owned(),
            imports: Vec::new(),
            imports_fingerprint:
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            effects: Vec::<AdmittedComponentEffect>::new(),
        };

        assert!(
            resolve_nested_target("tenant-a", std::slice::from_ref(&target), &dependency).is_some()
        );
        let mut mismatched = dependency.clone();
        mismatched.digest =
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_owned();
        assert!(
            resolve_nested_target("tenant-a", std::slice::from_ref(&target), &mismatched).is_none(),
            "a package/version match may not substitute another digest"
        );

        let declaration = AdmittedComponentOperation {
            registered_operation: None,
            dependencies: vec![dependency],
            input_ports: Vec::new(),
            output_ports: Vec::new(),
            parameters: Vec::new(),
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

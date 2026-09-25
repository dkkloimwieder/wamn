//! The single production driver for direct and queued wiring delivery.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::router_response::{PartialEvidence, PreparedResponse, ResponseState};
use crate::wiring_lowering::{WiringScope, lower_resolved_wiring};
use anyhow::Context as _;
use tracing::Instrument as _;
use wamn_catalog::{AdmittedComponent, DefinitionHash, ServingManifest, ServingWiring};
use wamn_engine::artifact_source::ComponentArtifactFetchErrorKind;
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::operation::native_workload::NativeComponent;
use wamn_engine::operation::{
    NativeApplication, OperationCall, OperationClosure, invoke_operation, node_types,
};
use wamn_engine::release_manifest::{LoadedRelease, validate_component_in_release};
use wamn_engine::router_delivery::{authorize_registered_operation, bounded_node_deadline_ms};
use wamn_event_wire::Causation;
use wamn_execution_host::{
    DeadlineAdjustment, InvocationSite, NativeFacts, NativePolicy, NodeAcquisition, OperationHost,
    WiringPreload, component_invocation, invocation_span, node_trace_context, remote_trace_context,
    synchronous_request_kind,
};
use wamn_project_state::PlatformComponent;
use wamn_router::{
    ActiveWiring, CacheInsert, Delivery, ErrorDetail, NodeError, NodeOutcome, Outcome,
    RateLimitDetail, Step, VersionKey, WiringCache, WiringCacheSnapshot,
};
use wamn_runtime::plugins::EffectEvidence;
use wamn_runtime::plugins::connection_http::{
    ConnectionExecutionClosure, InvocationEntry, WiringPosition,
};
use wamn_runtime::plugins::wamn_postgres::{
    CandidateBindingWorld, CandidateWiringResolution, ResolvedActiveWiring,
};

/// Shared CLI/environment key for the only wiring cache in a serving process.
pub const WIRING_CACHE_CAPACITY_ENV: &str = "WAMN_WIRING_CACHE_CAPACITY";

/// Default entries in the process-local wiring cache.
///
/// Entries are parsed documents plus immutable catalog pointers (roughly KiB),
/// while the production working set is hundreds of active wirings per
/// environment. 1,024 therefore costs single-digit MiB and cheaply avoids hot
/// path re-parsing; the hit/eviction metrics make the choice evidence-tunable.
pub const DEFAULT_WIRING_CACHE_CAPACITY: usize = 1_024;

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

/// Process-owned construction facts of the driver. The operation host holds
/// the project, schema, owner, and warm reuse facts.
#[derive(Debug, Clone)]
pub struct RouterDriverConfig {
    pub cache_capacity: WiringCacheCapacity,
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
    pub caller: Option<AuthenticatedCaller>,
    pub traceparent: Option<String>,
    pub tracestate: Option<String>,
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
    let trace = node_trace_context(
        request.traceparent.as_deref(),
        request.tracestate.as_deref(),
    );
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

fn remote_request_context(request: &RouterDriverRequest) -> Option<opentelemetry::Context> {
    remote_trace_context(
        request.traceparent.as_deref(),
        request.tracestate.as_deref(),
    )
}

fn component_invocation_span(
    request: &RouterDriverRequest,
    project: &str,
    wiring_version: u32,
    component_digest: &str,
    call: &wamn_router::NodeCall,
    remote_parent: Option<&opentelemetry::Context>,
) -> tracing::Span {
    invocation_span(
        &InvocationSite {
            tenant_id: &request.tenant_id,
            project,
            environment: &request.environment,
            wiring_id: &request.wiring_id,
            wiring_version,
            node_id: &call.node,
            input_port: call.input_port.as_deref(),
            operation: &call.operation,
            component_digest,
            caller: request.caller.as_ref(),
        },
        remote_parent,
    )
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

/// Deadline changes retained when execution returns an error.
#[derive(Debug)]
pub struct DeadlineAdjustments(pub Vec<DeadlineAdjustment>);

impl std::fmt::Display for DeadlineAdjustments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("execution deadline adjusted")
    }
}

impl std::error::Error for DeadlineAdjustments {}

/// One completely walked delivery, including the exact graph identity used.
#[derive(Debug, Clone)]
pub struct RouterDelivery {
    pub wiring_version: u32,
    pub graph_hash: Arc<str>,
    pub outcome: Outcome,
    pub deadline_adjustments: Vec<DeadlineAdjustment>,
    pub(crate) partial: Option<PartialEvidence>,
}

/// Read-only lifecycle totals for the bounded driver store.
#[derive(Debug, Clone)]
pub struct RouterDriverSnapshot {
    pub wiring_cache: WiringCacheSnapshot,
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
        application: &'a Arc<NativeApplication<NativePolicy>>,
    },
}

/// One router, cache, and artifact source per serving process. Both process
/// leaves construct this exact type.
pub struct RouterDriver {
    operations: Arc<OperationHost>,
    release: Arc<LoadedRelease>,
    config: RouterDriverConfig,
    cache: Arc<WiringCache<CatalogFacts>>,
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
    /// Run wiring nodes on the process's one operation host. The route path
    /// calls the same host, so a route and a wiring share one loaded
    /// application.
    pub fn new(operations: Arc<OperationHost>, config: RouterDriverConfig) -> Self {
        let cache = Arc::new(WiringCache::new(config.cache_capacity.get()));
        Self {
            release: Arc::clone(&operations.release),
            operations,
            config,
            cache,
            started: Instant::now(),
        }
    }

    /// The operation host this driver runs its nodes on.
    pub fn operations(&self) -> Arc<OperationHost> {
        Arc::clone(&self.operations)
    }

    pub fn snapshot(&self) -> RouterDriverSnapshot {
        RouterDriverSnapshot {
            wiring_cache: self.cache.snapshot(),
        }
    }

    /// Resolve and check every wiring that a synchronous attachment targets.
    /// The components come back for the release readiness to prepare.
    pub(crate) async fn preload_synchronous_wirings(&self) -> anyhow::Result<WiringPreload> {
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
                caller: None,
                traceparent: None,
                tracestate: None,
            };
            let active = self.resolve(&request).await.with_context(|| {
                format!("preload release wiring {wiring_id:?} version {wiring_version}")
            })?;
            self.validate_wiring_closure(&request, &active)?;
            for component in active.facts.components.iter() {
                self.validate_release_component(component)?;
            }
            components = Some(Arc::clone(&active.facts.components));
        }
        Ok(WiringPreload {
            wirings: targets.len(),
            components,
        })
    }

    /// Execute one queued delivery through the same router and node invoker.
    /// The caller owns acting on the terminal verdict.
    ///
    /// A callerless delivery executes as `wamn:executor`.
    pub async fn execute(&self, request: RouterDriverRequest) -> anyhow::Result<RouterDelivery> {
        self.execute_with_context(request, None, Some(PlatformComponent::Executor))
            .await
    }

    /// Execute one delivery with host-derived event provenance.
    ///
    /// Only the router-delivery bridge can mint this context. It is distinct
    /// from caller identity: a post-commit registration remains callerless
    /// while every PostgreSQL transaction it drives carries the delivery's
    /// causation stamp.
    ///
    /// `platform` names the component that executes a callerless delivery.
    pub(crate) async fn execute_with_causation(
        &self,
        request: RouterDriverRequest,
        causation: Causation,
        platform: Option<PlatformComponent>,
    ) -> anyhow::Result<RouterDelivery> {
        self.execute_with_context(request, Some(causation), platform)
            .await
    }

    async fn execute_with_context(
        &self,
        request: RouterDriverRequest,
        causation: Option<Causation>,
        platform: Option<PlatformComponent>,
    ) -> anyhow::Result<RouterDelivery> {
        self.validate_request_scope(&request)?;
        let active = self
            .resolve(&request)
            .instrument(tracing::info_span!("wamn.router.resolve"))
            .await?;
        self.validate_wiring_closure(&request, &active)?;
        self.execute_resolved(
            request,
            active,
            ExecutionClosure::Released,
            causation,
            platform,
        )
        .await
    }

    /// Execute a DB-frozen candidate through the same router and invoker as
    /// release-backed delivery. A candidate case executes as `wamn:executor`.
    pub async fn execute_candidate(
        &self,
        request: CandidateCaseRequest,
    ) -> anyhow::Result<RouterDelivery> {
        self.validate_candidate_target(&request.target)?;
        let active = self
            .resolve_candidate(&request.target, &request.binding_world)
            .instrument(tracing::info_span!("wamn.router.resolve"))
            .await?;
        Self::validate_candidate_closure(&request.target, &active)?;
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
        let application = self.operations.load_application(native).await?;
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
                Some(PlatformComponent::Executor),
            )
            .await;
        let cleanup = application.workload.unbind_all_plugins().await;
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
        platform: Option<PlatformComponent>,
    ) -> anyhow::Result<RouterDelivery> {
        let mut deadline_adjustments = Vec::new();
        let result = async {
            // Parse the ingress context once per delivery, not once per node on the
            // router hot path. Queue delivery deliberately carries no remote
            // context and inherits the executor's host-created queue root instead.
            let remote_parent = remote_request_context(&request);
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
                            deadline_adjustments: Vec::new(),
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
                            if let Some(requested_ms) = call.deadline_ms {
                                let effective_ms = bounded_node_deadline_ms(Some(requested_ms));
                                if requested_ms != effective_ms {
                                    deadline_adjustments.push(DeadlineAdjustment {
                                        node: call.node.clone(),
                                        requested_ms,
                                        effective_ms,
                                    });
                                }
                            }
                            let span = component_invocation_span(
                                &request,
                                &self.operations.project,
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
                                    platform,
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
                                        deadline_adjustments: Vec::new(),
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
        }.await;
        match result {
            Ok(mut delivery) => {
                delivery.deadline_adjustments = deadline_adjustments;
                Ok(delivery)
            }
            Err(error) if !deadline_adjustments.is_empty() => {
                Err(error.context(DeadlineAdjustments(deadline_adjustments)))
            }
            Err(error) => Err(error),
        }
    }

    async fn resolve_candidate(
        &self,
        target: &CandidateWiringTarget,
        expected_binding_world: &CandidateBindingWorld,
    ) -> anyhow::Result<ActiveWiring<CatalogFacts>> {
        let resolved = self
            .operations
            .postgres
            .resolve_candidate_wiring(
                &self.operations.project,
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
            CandidateWiringResolution::Resolved(resolved) => *resolved,
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
        let scope = WiringScope {
            tenant_id: &target.tenant_id,
            package_id: &target.package_id,
            environment: &target.environment,
        };
        let wiring = lower_resolved_wiring(scope, &resolved).map_err(|_| {
            CandidateExecutionRefusal::new(
                CandidateExecutionRefusalKind::Definition,
                "candidate-definition-invalid",
            )
        })?;
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
            VersionKey {
                tenant_id: &target.tenant_id,
                package_id: &target.package_id,
                environment: &target.environment,
                effective_release_id: target.effective_release_id,
                wiring_id: &target.wiring_id,
                version: resolved.version,
            },
            Arc::clone(&resolved.graph_hash),
            wiring,
            facts,
        ) {
            CacheInsert::Installed(active) => Ok(active),
            CacheInsert::HashMismatch => Err(CandidateExecutionRefusal::new(
                CandidateExecutionRefusalKind::Identity,
                "candidate-wiring-immutable-hash-mismatch",
            )
            .into()),
        }
    }

    async fn resolve(
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
            .operations
            .postgres
            .resolve_release_wiring(
                &self.operations.project,
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
        let scope = WiringScope {
            tenant_id: &request.tenant_id,
            package_id: &request.package_id,
            environment: &request.environment,
        };
        let wiring = lower_resolved_wiring(scope, &resolved).context("lower active wiring")?;
        let facts = CatalogFacts::from_resolved(&resolved)?;
        match self.cache.insert_version(
            VersionKey {
                tenant_id: &request.tenant_id,
                package_id: &request.package_id,
                environment: &request.environment,
                effective_release_id,
                wiring_id: &request.wiring_id,
                version: resolved.version,
            },
            Arc::clone(&resolved.graph_hash),
            wiring,
            facts,
        ) {
            CacheInsert::Installed(active) => Ok(active),
            CacheInsert::HashMismatch => {
                anyhow::bail!("release-wiring-immutable-hash-mismatch")
            }
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
            match self.operations.source.pull_verified(component).await {
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
            self.release.manifest().workflow.wirings.contains(&expected),
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

    #[expect(
        clippy::too_many_arguments,
        reason = "the delivery, graph, call, closure, provenance and executing component are independent facts"
    )]
    async fn invoke_node(
        &self,
        request: &RouterDriverRequest,
        active: &ActiveWiring<CatalogFacts>,
        call: &wamn_router::NodeCall,
        closure: ExecutionClosure<'_>,
        causation: Option<&Causation>,
        platform: Option<PlatformComponent>,
        effects: Option<EffectEvidence>,
    ) -> anyhow::Result<NodeOutcome> {
        let component = active
            .facts
            .component(&call.node)
            .ok_or_else(|| anyhow::anyhow!("router-node-component-fact-missing"))?;
        if matches!(closure, ExecutionClosure::Released) {
            self.validate_release_component(component)?;
        }
        let release = matches!(closure, ExecutionClosure::Released)
            .then(|| self.operations.release_identity());
        let (operation_closure, connection_closure) = match closure {
            ExecutionClosure::Released => (
                OperationClosure::Released(&active.facts.components),
                ConnectionExecutionClosure::Released,
            ),
            ExecutionClosure::Candidate {
                target,
                binding_world,
                application,
            } => (
                OperationClosure::Candidate(application),
                ConnectionExecutionClosure::Candidate {
                    effective_release_id: target.effective_release_id,
                    environment: target.environment.clone(),
                    wiring_hash: target.wiring_hash.clone(),
                    component: component.component.clone(),
                    interface_version: component.interface_version.clone(),
                    binding_world: Arc::clone(binding_world),
                },
            ),
        };
        let entry = InvocationEntry::Wiring(WiringPosition {
            package_id: request.package_id.clone(),
            wiring_id: request.wiring_id.clone(),
            wiring_version: active.version,
            node_id: call.node.clone(),
            occurrence: call.occurrence,
        });
        let deadline_ms = bounded_node_deadline_ms(call.deadline_ms);
        let call = OperationCall {
            closure: operation_closure,
            component,
            operation: &call.operation,
            context: node_context(request, active.version, call, deadline_ms)?,
            input: &call.payload,
            deadline_ms,
            facts: NativeFacts::entry(
                NodeAcquisition {
                    claims: self.operations.claims(&request.tenant_id, release),
                    invocation: component_invocation(
                        component,
                        &call.operation,
                        entry,
                        connection_closure,
                        effects,
                    ),
                    causation: causation.cloned(),
                    platform,
                },
                request.caller.clone(),
            ),
        };
        // Boxed, so each node's call does not grow the delivery future.
        Box::pin(invoke_operation(&*self.operations, call, None))
            .await
            .and_then(lower_node_outcome)
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

fn synchronous_wiring_targets(manifest: &ServingManifest) -> BTreeSet<(String, String, u32)> {
    manifest
        .workflow
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
    use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use opentelemetry_sdk::trace::{
        InMemorySpanExporter, InMemorySpanExporterBuilder, SdkTracerProvider, SpanData,
    };
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    use tracing_subscriber::layer::SubscriberExt as _;
    use wamn_catalog::{
        EffectiveReleaseId, PackageCoordinate, SERVING_MANIFEST_FORMAT_VERSION, ServingAttachment,
        ServingRegistration, ServingRegistrationInput, ServingRelease,
    };

    use super::*;
    use wamn_catalog::AttachmentKind;
    use wamn_execution_host::synchronous_route_count;

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
            operation: "orders:widget/get@1.0.0".to_owned(),
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
            target: wamn_catalog::AttachmentTarget::Wiring {
                wiring_id: wiring_id.to_owned(),
                wiring_version: 3,
            },
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
    fn component_span_adopts_remote_traceparent_and_host_identity() {
        let harness = TraceHarness::install();
        let request = driver_request(Some(VALID_TRACEPARENT));
        let parent = remote_request_context(&request).expect("valid W3C parent must extract");
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
        let parent = remote_request_context(&request).expect("valid W3C parent must extract");
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
        assert!(remote_request_context(&request).is_none());
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
        let (attachments, workflow_attachments) = ServingAttachment::split(BTreeMap::from([
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
            (
                "route".to_owned(),
                ServingAttachment {
                    target: wamn_catalog::AttachmentTarget::Route {
                        component: "orders".to_owned(),
                        operation: "orders:order/get@1.0.0".to_owned(),
                    },
                    ..attachment(AttachmentKind::Http, "unused")
                },
            ),
        ]));
        let manifest = ServingManifest {
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release: ServingRelease {
                tenant_id: "tenant-a".to_owned(),
                effective_release_id: EffectiveReleaseId::new(7).unwrap(),
                environment: "prod".to_owned(),
                packages: BTreeSet::from([PackageCoordinate::new("orders", "1.0.0").unwrap()]),
            },
            components: BTreeSet::new(),
            routes: BTreeSet::new(),
            attachments,
            workflow: wamn_catalog::WorkflowSection {
                wirings: BTreeSet::new(),
                attachments: workflow_attachments,
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
            },
        };

        assert_eq!(
            synchronous_wiring_targets(&manifest),
            BTreeSet::from([
                ("orders".to_owned(), "request-wiring".to_owned(), 3),
                ("orders".to_owned(), "studio-wiring".to_owned(), 3),
            ])
        );
        assert_eq!(synchronous_route_count(&manifest), 1);
    }
}

//! The single production driver for direct and queued wiring delivery.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::wiring_lowering::{WiringScope, lower_resolved_wiring};
use anyhow::Context as _;
use tracing::Instrument as _;
use wamn_catalog::AdmittedComponent;
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::operation::{OperationCall, OperationClosure, invoke_operation, node_types};
use wamn_engine::release_manifest::{LoadedRelease, validate_component_in_release};
use wamn_engine::router_delivery::{
    OperationRefusal, OperationRefusalType, authorize_registered_operation,
    bounded_node_deadline_ms,
};
use wamn_event_wire::Causation;
use wamn_execution_host::{
    CallIntents, DeadlineAdjustment, InvocationSite, NativeFacts, NodeAcquisition, OperationHost,
    component_invocation, invocation_span, node_trace_context, remote_trace_context,
};
use wamn_project_state::PlatformComponent;
use wamn_router::{
    ActiveWiring, Delivery, ErrorDetail, NodeError, NodeOutcome, Outcome, RateLimitDetail, Step,
};
use wamn_runtime::plugins::EffectEvidence;
use wamn_runtime::plugins::connection_http::{
    ConnectionExecutionClosure, InvocationEntry, WiringPosition,
};
use wamn_runtime::plugins::wamn_postgres::{ResolvedActiveWiring, WorkflowRunKey};

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

/// One queued run of an environment workflow (docs/plan/workflows.md R3,
/// epic 1), with the facts its run row froze at admission.
///
/// `run` carries the application as its package, the workflow id and version
/// as its wiring coordinate, and the claim-time caller. The run executes on a
/// host of the release it pins: the claim leases it only there.
#[derive(Debug, Clone)]
pub struct WorkflowRunRequest {
    pub run: RouterDriverRequest,
    pub wiring_hash: String,
    /// The component digest admission froze for each node.
    pub node_digests: BTreeMap<String, String>,
    /// The principal the run executes as, and its one active role.
    pub identity: WorkflowIdentity,
    pub causation: Option<Causation>,
}

/// The principal a workflow run executes as, with its one active role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowIdentity {
    pub principal_id: String,
    pub role: String,
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
}

#[derive(Debug, PartialEq, Eq)]
struct CatalogFacts {
    manifest_digest: String,
    components: Arc<[AdmittedComponent]>,
    node_components: Arc<BTreeMap<String, AdmittedComponent>>,
}

impl CatalogFacts {
    fn from_resolved(resolved: &ResolvedActiveWiring) -> Self {
        Self {
            manifest_digest: resolved.manifest_digest.clone(),
            components: Arc::clone(&resolved.components),
            node_components: Arc::clone(&resolved.node_components),
        }
    }

    fn component(&self, node: &str) -> Option<&AdmittedComponent> {
        self.node_components.get(node)
    }
}

/// The workflow router for the serving process.
pub struct RouterDriver {
    operations: Arc<OperationHost>,
    release: Arc<LoadedRelease>,
    started: Instant,
}

impl fmt::Debug for RouterDriver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RouterDriver")
            .finish_non_exhaustive()
    }
}

impl RouterDriver {
    /// Run wiring nodes on the process's one operation host. The route path
    /// calls the same host, so a route and a wiring share one loaded
    /// application.
    pub fn new(operations: Arc<OperationHost>) -> Self {
        Self {
            release: Arc::clone(&operations.release),
            operations,
            started: Instant::now(),
        }
    }

    /// The operation host this driver runs its nodes on.
    pub fn operations(&self) -> Arc<OperationHost> {
        Arc::clone(&self.operations)
    }

    /// Execute one queued environment workflow, resolving its pinned row on
    /// every claim. Each node uses the current grants of its execution role.
    pub async fn execute_workflow(
        &self,
        request: WorkflowRunRequest,
    ) -> anyhow::Result<RouterDelivery> {
        let WorkflowRunRequest {
            run,
            wiring_hash,
            node_digests,
            identity,
            causation,
        } = request;
        // A workflow run's package id is its application, a name the
        // environment declares with its packages, never a package.
        let scope = self.release.scope();
        anyhow::ensure!(run.wiring_version > 0, "wiring-version-zero");
        anyhow::ensure!(
            scope.tenant_id == run.tenant_id && scope.environment == run.environment,
            "router-request-release-scope-mismatch"
        );
        let application_packages = &scope
            .applications
            .get(&run.package_id)
            .with_context(|| {
                format!(
                    "the environment declares no application {:?}",
                    run.package_id
                )
            })?
            .packages;
        let manifest_digest = self.release.release().manifest_digest.as_str();
        let resolved = self
            .operations
            .postgres
            .resolve_workflow(
                &self.operations.project,
                &WorkflowRunKey {
                    tenant_id: &run.tenant_id,
                    environment: &run.environment,
                    application: &run.package_id,
                    application_packages,
                    workflow_id: &run.wiring_id,
                    version: run.wiring_version,
                    manifest_digest,
                    wiring_hash: &wiring_hash,
                    node_digests: &node_digests,
                },
            )
            .instrument(tracing::info_span!("wamn.router.resolve"))
            .await?;
        let scope = WiringScope {
            tenant_id: &run.tenant_id,
            package_id: &run.package_id,
            environment: &run.environment,
        };
        let wiring = lower_resolved_wiring(scope, &resolved).context("lower workflow graph")?;
        let active = ActiveWiring {
            version: resolved.version,
            graph_hash: Arc::clone(&resolved.graph_hash),
            wiring: Arc::new(wiring),
            facts: Arc::new(CatalogFacts::from_resolved(&resolved)),
        };
        self.execute_resolved(
            run,
            active,
            causation,
            Some(PlatformComponent::Executor),
            Some(&identity),
        )
        .await
    }

    /// `workflow` is the identity of a workflow run: each node is then
    /// authorized with the grants the principal holds for its role now. A
    /// released or candidate delivery keeps its claim-time caller.
    async fn execute_resolved(
        &self,
        request: RouterDriverRequest,
        active: ActiveWiring<CatalogFacts>,
        causation: Option<Causation>,
        platform: Option<PlatformComponent>,
        workflow: Option<&WorkflowIdentity>,
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
            loop {
                let now_ms = self.now_ms();
                match wiring.next(&mut walk, now_ms) {
                    Step::Done(status) => {
                        return Ok(RouterDelivery {
                            wiring_version: active.version,
                            graph_hash: Arc::clone(&active.graph_hash),
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
                        let effects = None;
                        let result = async {
                            let component = active
                                .facts
                                .component(&call.node)
                                .ok_or_else(|| anyhow::anyhow!("router-node-component-fact-missing"))?;
                            let operation = component
                                .operation(&call.operation)
                                .ok_or_else(|| anyhow::anyhow!("router-node-operation-fact-missing"))?;
                            let caller = match workflow {
                                Some(identity) => Some(
                                    self.workflow_node_caller(&request, identity, operation, &call)
                                        .await?,
                                ),
                                None => request.caller.clone(),
                            };
                            authorize_registered_operation(
                                caller.as_ref(),
                                operation.registered_operation.as_deref(),
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
                                    causation.as_ref(),
                                    platform,
                                    effects.clone(),
                                    caller,
                                )
                                .instrument(span)
                                .await
                                .with_context(|| format!("invoke wiring node {:?}", call.node))?;
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
                                return Err(error);
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

    /// The caller of one node of a workflow run: the principal with the
    /// grants of its role, read now through the host's auth memory. A
    /// principal that is inactive or no longer holds the role is refused the
    /// node, as a request is refused an operation it does not hold.
    async fn workflow_node_caller(
        &self,
        request: &RouterDriverRequest,
        identity: &WorkflowIdentity,
        operation: &wamn_catalog::AdmittedComponentOperation,
        call: &wamn_router::NodeCall,
    ) -> anyhow::Result<AuthenticatedCaller> {
        self.operations
            .postgres
            .workflow_node_caller(
                &self.operations.project,
                &request.tenant_id,
                &identity.principal_id,
                &identity.role,
            )
            .await?
            .ok_or_else(|| {
                OperationRefusal::new(
                    OperationRefusalType::PermissionDenied,
                    operation
                        .registered_operation
                        .as_deref()
                        .unwrap_or(&call.operation),
                )
                .into()
            })
    }

    fn validate_release_component(&self, component: &AdmittedComponent) -> anyhow::Result<()> {
        validate_component_in_release(&self.release, component)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the delivery, graph, call, closure, provenance, executing component and caller are independent facts"
    )]
    async fn invoke_node(
        &self,
        request: &RouterDriverRequest,
        active: &ActiveWiring<CatalogFacts>,
        call: &wamn_router::NodeCall,
        causation: Option<&Causation>,
        platform: Option<PlatformComponent>,
        effects: Option<EffectEvidence>,
        caller: Option<AuthenticatedCaller>,
    ) -> anyhow::Result<NodeOutcome> {
        let component = active
            .facts
            .component(&call.node)
            .ok_or_else(|| anyhow::anyhow!("router-node-component-fact-missing"))?;
        self.validate_release_component(component)?;
        let release = Some(self.operations.release_identity());
        let operation_closure = OperationClosure::Released(&active.facts.components);
        let connection_closure = ConnectionExecutionClosure::Released;
        let entry = InvocationEntry::Wiring(WiringPosition {
            package_id: request.package_id.clone(),
            wiring_hash: active.graph_hash.to_string(),
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
                caller,
            ),
        };
        // A node logs the intents of the route that its operation publishes.
        let intents = self
            .operations
            .call_intents(component, call.operation)
            .map_err(|error| error.context("build the intents of a node call"))?;
        let intent = intents.as_ref().map(CallIntents::context);
        // Boxed, so each node's call does not grow the delivery future.
        Box::pin(invoke_operation(&*self.operations, call, intent))
            .await
            .and_then(lower_node_outcome)
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
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
}

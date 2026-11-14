//! Walk a pinned workflow through the shared operation entry.
use crate::graph::{
    Delivery, ErrorDetail, NodeError, NodeOutcome, Outcome, RateLimitDetail, Step, Wiring,
};
use anyhow::Context as _;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::Instrument as _;
use wamn_engine::operation::{OperationCall, OperationClosure, invoke_operation, node_types};
use wamn_engine::release_manifest::validate_component_in_release;
use wamn_engine::router_delivery::{
    OperationRefusal, OperationRefusalType, authorize_registered_operation,
    bounded_node_deadline_ms,
};
use wamn_event_wire::Causation;
use wamn_execution_host::{
    CallIntents, DeadlineAdjustment, InvocationSite, NativeFacts, NodeAcquisition, OperationHost,
    component_invocation, invocation_span, node_trace_context,
};
use wamn_identity_protocol::PlatformComponent;
use wamn_runtime::plugins::connection_http::{
    ConnectionExecutionClosure, InvocationEntry, WiringPosition,
};
use wamn_runtime::plugins::wamn_postgres::{ResolvedActiveWiring, WorkflowRunKey};

/// The pinned graph and execution identity of one queued workflow.
#[derive(Debug)]
pub struct WorkflowRunRequest {
    pub project_env_key: String,
    pub package_id: String,
    pub environment: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub delivery_id: String,
    pub payload: serde_json::Value,
    pub wiring_hash: String,
    pub node_digests: BTreeMap<String, String>,
    pub identity: WorkflowIdentity,
    pub causation: Option<Causation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowIdentity {
    pub principal_id: String,
    pub role: String,
}

pub struct RouterDriver {
    operations: Arc<OperationHost>,
    started: Instant,
}

impl fmt::Debug for RouterDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RouterDriver").finish_non_exhaustive()
    }
}

impl RouterDriver {
    pub fn new(operations: Arc<OperationHost>) -> Self {
        Self {
            operations,
            started: Instant::now(),
        }
    }
    pub fn operations(&self) -> Arc<OperationHost> {
        Arc::clone(&self.operations)
    }

    /// Return deadline adjustments even if resolution or invocation fails.
    pub async fn execute_workflow(
        &self,
        request: WorkflowRunRequest,
    ) -> (anyhow::Result<Outcome>, Vec<DeadlineAdjustment>) {
        let mut adjustments = Vec::new();
        let result = async {
            let target = self.operations.release.target();
            anyhow::ensure!(request.wiring_version > 0, "wiring-version-zero");
            anyhow::ensure!(target.project_env_key == request.project_env_key && target.environment == request.environment, "router-request-release-target-mismatch");
            let application_packages = &target.applications.get(&request.package_id)
                .with_context(|| format!("the environment declares no application {:?}", request.package_id))?.packages;
            let ResolvedActiveWiring { version, graph_hash, document, node_components, components, .. } = self.operations.postgres.resolve_workflow(
                &self.operations.project,
                &WorkflowRunKey {
                    project_env_key: &request.project_env_key, environment: &request.environment,
                    application: &request.package_id, application_packages,
                    workflow_id: &request.wiring_id, version: request.wiring_version,
                    manifest_digest: self.operations.release.release().manifest_digest.as_str(),
                    wiring_hash: &request.wiring_hash, node_digests: &request.node_digests,
                }).instrument(tracing::info_span!("wamn.router.resolve")).await?;
            for component in node_components.values() {
                validate_component_in_release(&self.operations.release, component)?;
            }
            let wiring = Wiring::compile(document, &node_components)?;
            let mut walk = wiring.start(Delivery { payload: request.payload, caller_attached: false });
            loop {
                match wiring.next(&mut walk, self.now_ms()) {
                    Step::Done(_) => return Ok(walk.into_outcome()),
                    Step::Wait { until_ms } => tokio::time::sleep(Duration::from_millis(until_ms.saturating_sub(self.now_ms()))).await,
                    Step::Invoke(call) => {
                        let result = async {
                            let component = &node_components[&call.node];
                            let operation_name = &call.definition.operation;
                            let operation = component.operation(operation_name).expect("resolved operation exists");
                            let caller = self.operations.postgres.workflow_node_caller(
                                &self.operations.project, &request.project_env_key,
                                &request.identity.principal_id, &request.identity.role,
                            ).await?.ok_or_else(|| OperationRefusal::new(OperationRefusalType::PermissionDenied,
                                operation.registered_operation.as_deref().unwrap_or(operation_name)))?;
                            authorize_registered_operation(Some(&caller), operation.registered_operation.as_deref())?;
                            let deadline_ms = bounded_node_deadline_ms(call.deadline_ms);
                            if let Some(requested_ms) = call.deadline_ms.filter(|requested| *requested != deadline_ms) {
                                adjustments.push(DeadlineAdjustment { node: call.node.clone(), requested_ms, effective_ms: deadline_ms });
                            }
                            let span = invocation_span(&InvocationSite {
                                project_env_key: &request.project_env_key, project: &self.operations.project,
                                environment: &request.environment, wiring_id: &request.wiring_id, wiring_version: version,
                                node_id: &call.node, input_port: call.input_port.as_deref(), operation: operation_name,
                                component_digest: &component.component_digest, caller: Some(&caller),
                            }, None);
                            async {
                                let trace = node_trace_context(None, None);
                                let context = node_types::NodeContext {
                                    wiring_id: request.wiring_id.clone(), wiring_version: version, node_id: call.node.clone(),
                                    delivery_id: request.delivery_id.clone(), input_port: call.input_port.clone(), occurrence: call.occurrence,
                                    traceparent: trace.traceparent, tracestate: trace.tracestate, deadline_ms: Some(deadline_ms),
                                    config: serde_json::to_string(&call.definition.params).context("encode node config")?,
                                };
                                let entry = InvocationEntry::Wiring(WiringPosition {
                                    package_id: request.package_id.clone(), wiring_hash: graph_hash.to_string(),
                                    wiring_id: request.wiring_id.clone(), wiring_version: version, node_id: call.node.clone(), occurrence: call.occurrence,
                                });
                                let operation_call = OperationCall {
                                    closure: OperationClosure::Released(&components), component, operation: operation_name,
                                    context, input: &call.payload, deadline_ms,
                                    facts: NativeFacts::entry(NodeAcquisition {
                                        identity: self.operations.identity(&request.project_env_key, Some(self.operations.release_identity())),
                                        invocation: component_invocation(component, operation_name, entry, ConnectionExecutionClosure::Released, None),
                                        causation: request.causation.clone(), platform: Some(PlatformComponent::Executor),
                                    }, Some(caller)),
                                };
                                let intents = self.operations.call_intents(component, operation_name)?;
                                Box::pin(invoke_operation(&*self.operations, operation_call, intents.as_ref().map(CallIntents::context))).await.and_then(lower_node_outcome)
                            }.instrument(span).await
                        }.await;
                        let outcome = match result {
                            Ok(outcome) => outcome,
                            Err(error) if walk.verdict().is_some() => {
                                tracing::warn!(error = %format_args!("{error:#}"), "workflow invocation failed after its terminal verdict; first verdict stands");
                                drop(call);
                                let mut outcome = walk.into_outcome();
                                outcome.status = crate::graph::WalkStatus::Failed;
                                return Ok(outcome);
                            }
                            Err(error) => return Err(error),
                        };
                        wiring.apply(&mut walk, &call, outcome, self.now_ms()).context("apply workflow transition")?;
                    }
                }
            }
        }.await;
        (result, adjustments)
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
                    .unwrap_or_else(|| wamn_catalog::MAIN_PORT.to_owned()),
            })
        }
        Err(node_types::NodeError::Retryable(detail)) => Ok(NodeOutcome::Error(
            NodeError::Retryable(lower_detail(detail)),
        )),
        Err(node_types::NodeError::RateLimited(detail)) => Ok(NodeOutcome::Error(
            NodeError::RateLimited(RateLimitDetail {
                detail: lower_detail(detail.detail),
                retry_after_ms: detail.retry_after_ms,
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

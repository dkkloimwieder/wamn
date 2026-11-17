//! Dispatch a node through the exact sibling's SQL and connection authority.

use super::{SharedCtx, WasmResult, Workflow, Workload, service, wit_node};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::time::Instant;
use wamn_catalog::canonical_name::CanonicalName;
use wamn_connection::connection::WiringPosition;
use wamn_engine::native_routes::{OperationRefusal, bounded_node_deadline_ms};
use wamn_engine::operation::node_types;
use wamn_run_state::persistence;
use wamn_sql::WorkflowNode;
use wash_runtime::wasmtime::component::Accessor;
use wit_node::{Detail, Emission, Invocation, NodeError};

impl Workflow {
    async fn invoke_node(
        &self,
        workload: &Workload,
        request: Invocation,
    ) -> Result<Emission, NodeError> {
        let authority = workload.authority(&request.handle).map_err(|_| {
            terminal(
                "invalid-authority",
                "workflow authority has expired or is unknown",
            )
        })?;
        let mut deadline = Instant::now()
            + Duration::from_millis(bounded_node_deadline_ms(Some(request.remaining_ms)));
        if request.remaining_ms == 0 {
            return Err(NodeError::Cancelled);
        }
        if let Some(request_deadline) = authority.request_deadline
            && !authority.responded.load(Ordering::Acquire)
        {
            deadline = deadline.min(request_deadline);
        }
        if let Some(generation) = authority.generation {
            let checked_at = Instant::now();
            let remaining = self
                .postgres
                .executor_write(&workload.application.project, async |connection| {
                    persistence::lease_remaining_ms(
                        connection,
                        &authority.delivery,
                        &workload.runner,
                        generation,
                    )
                    .await
                })
                .await
                .map_err(|error| {
                    tracing::warn!(%error, "check workflow node lease");
                    retryable("storage", "the workflow store is unavailable")
                })?;
            let Some(remaining) = remaining.and_then(|ms| u64::try_from(ms).ok()) else {
                workload.remove_authority(&request.handle);
                return Err(terminal(
                    "fence-lost",
                    "the workflow no longer holds this lease",
                ));
            };
            deadline = deadline.min(checked_at + Duration::from_millis(remaining));
        }
        if deadline <= Instant::now() {
            return Err(NodeError::Cancelled);
        }
        let operation = CanonicalName::try_from(request.export)
            .map_err(|error| invalid("invalid-export", error.to_string()))?;
        let node_id = CanonicalName::try_from(request.node_id)
            .map_err(|error| invalid("invalid-node", error.to_string()))?;
        let config: BTreeMap<CanonicalName, serde_json::Value> =
            serde_json::from_str(&request.config_json)
                .map_err(|error| invalid("invalid-node-config", error.to_string()))?;
        if !workload.application.components.iter().any(|component| {
            component.name == request.sibling && component.operations.contains_key(&operation)
        }) {
            return Err(terminal(
                "unresolvable-name",
                "the sibling does not export this operation",
            ));
        }
        serde_json::from_str::<serde::de::IgnoredAny>(&request.input_json)
            .map_err(|error| invalid("invalid-input", error.to_string()))?;
        let invoke = async {
            let caller = self
                .postgres
                .workflow_node_caller(
                    &workload.application.project,
                    &workload.project_key,
                    &authority.principal,
                    &authority.role,
                )
                .await
                .map_err(|error| {
                    tracing::warn!(%error, "read workflow execution role");
                    retryable(
                        "authority-unavailable",
                        "the execution role could not be read",
                    )
                })?
                .ok_or_else(|| {
                    terminal("permission-denied", "the execution role is unavailable")
                })?;
            let context = node_types::NodeContext {
                wiring_id: String::from(authority.workflow_id.as_str()),
                wiring_version: authority.version,
                node_id: String::from(node_id.as_str()),
                delivery_id: authority.delivery.clone(),
                input_port: request.input_port,
                occurrence: request.occurrence,
                traceparent: None,
                tracestate: None,
                deadline_ms: Some(super::entry::remaining_ms(deadline)),
                config: request.config_json,
            };
            let causation = authority.causation.clone().or_else(|| {
                Some(wamn_event_wire::Causation {
                    run: authority.delivery.clone(),
                    root: authority.delivery.clone(),
                    depth: 0,
                })
            });
            let sibling = request.sibling.as_str();
            let outcome = self
                .postgres
                .dispatch_workflow_node(
                    WorkflowNode {
                        route: &workload.route,
                        sibling: &request.sibling,
                        operation: &operation,
                        caller,
                        position: WiringPosition {
                            wiring_hash: authority.hash,
                            application: workload.application.application.clone(),
                            wiring_id: authority.workflow_id.clone(),
                            wiring_version: authority.version,
                            node_id,
                            occurrence: request.occurrence,
                            configuration: Arc::new(config),
                        },
                        causation,
                    },
                    request.input_json,
                    deadline,
                    |scope, input| {
                        let scope = Arc::from(scope);
                        let mut context = context.clone();
                        let operation = operation.clone();
                        context.deadline_ms = Some(super::entry::remaining_ms(deadline));
                        async move {
                            self.operations
                                .invoke_sibling(
                                    &workload.route,
                                    sibling,
                                    &self.postgres.scopes,
                                    scope,
                                    operation,
                                    context,
                                    input,
                                )
                                .await
                        }
                    },
                )
                .await
                .map_err(|error| {
                    if error.downcast_ref::<OperationRefusal>().is_some() {
                        terminal(
                            "permission-denied",
                            "the execution role does not hold this operation",
                        )
                    } else {
                        tracing::warn!(%error, "dispatch workflow node");
                        terminal("execution-failed", "the node did not return a result")
                    }
                })?;
            outcome
                .map(|emission| Emission {
                    payload_json: emission.payload,
                    port: emission.port,
                })
                .map_err(lower_error)
        };
        tokio::time::timeout_at(deadline, invoke)
            .await
            .unwrap_or(Err(NodeError::Cancelled))
    }
}

impl wit_node::Host for wash_runtime::engine::ctx::ActiveCtx<'_> {}

impl<T: Send + 'static> wit_node::HostWithStore<T> for SharedCtx {
    async fn call(
        accessor: &Accessor<T, Self>,
        request: Invocation,
    ) -> WasmResult<Result<Emission, NodeError>> {
        let Ok((plugin, workload)) = service(accessor)? else {
            return Ok(Err(terminal(
                "invalid-authority",
                "the workflow service is not bound",
            )));
        };
        Ok(plugin.invoke_node(&workload, request).await)
    }
}

fn lower_error(error: node_types::NodeError) -> NodeError {
    fn detail(detail: node_types::ErrorDetail) -> Detail {
        Detail {
            code: detail.code,
            message: detail.message,
        }
    }
    match error {
        node_types::NodeError::Retryable(error) => NodeError::Retryable(detail(error)),
        node_types::NodeError::RateLimited(error) => NodeError::RateLimited(wit_node::Throttle {
            error: detail(error.detail),
            retry_after_ms: error.retry_after_ms,
        }),
        node_types::NodeError::Terminal(error) => NodeError::Terminal(detail(error)),
        node_types::NodeError::InvalidInput(error) => NodeError::InvalidInput(detail(error)),
        node_types::NodeError::Cancelled => NodeError::Cancelled,
    }
}

fn invalid(code: &str, message: String) -> NodeError {
    NodeError::InvalidInput(Detail {
        code: Some(code.into()),
        message,
    })
}

fn terminal(code: &str, message: &str) -> NodeError {
    NodeError::Terminal(Detail {
        code: Some(code.into()),
        message: message.into(),
    })
}

fn retryable(code: &str, message: &str) -> NodeError {
    NodeError::Retryable(Detail {
        code: Some(code.into()),
        message: message.into(),
    })
}

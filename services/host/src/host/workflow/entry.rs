//! Native dispatch into the workflow service's entry exports.

use super::admission::{Admission, Definition, Delivery};
use super::bindings::{Service, exports::wamn::workflow::entry};
use super::{Authority, Refusal, SharedCtx, Workflow, storage_error, wit_run};
use serde_json::Value;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::Instant;
use wamn_application::ingress::AuthenticatedCaller;
use wamn_identity_protocol::{PrincipalId, workflow_user_id};
use wamn_run_state::admission::RunTrigger;
use wamn_run_state::run_store::ProductionLeaseError;
use wash_runtime::engine::dispatch::{GuestCall, GuestCallFuture};
use wash_runtime::wasmtime::component::{Accessor, Instance};

/// Facts selected by the native Request route.
pub(crate) struct RequestCall<'a> {
    pub workload: &'a str,
    pub path: &'a str,
    pub caller: &'a AuthenticatedCaller,
    pub input: Value,
    pub deadline: Instant,
}

/// Admin-selected principal for an immediate durable run.
#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunAs {
    Admin,
    Workflow,
}

/// Facts selected by the native run-now route.
pub(crate) struct RunNow<'a> {
    pub workload: &'a str,
    pub workflow_id: &'a str,
    pub caller: &'a AuthenticatedCaller,
    pub run_as: RunAs,
    pub input: &'a Value,
    pub idempotency_key: &'a str,
}

impl Workflow {
    /// Admit a Request under its execution policy and dispatch its synchronous entry.
    pub(crate) async fn call(
        &self,
        request: RequestCall<'_>,
    ) -> Result<Option<entry::Response>, Refusal> {
        let workload = self.workload(request.workload)?;
        let app = &workload.application;
        let prepare = self
            .postgres
            .executor_write(&app.project, async |connection| {
                let row = sqlx::query(
                "SELECT workflow_id, version, graph_json::text AS graph, graph_hash, execution_role
                   FROM catalog.workflows WHERE environment = $1 AND application = $2
                    AND enabled AND trigger = 'request' AND path = $3 FOR SHARE"
            ).bind(&app.environment).bind(&app.application).bind(request.path)
                .fetch_optional(&mut *connection).await
                .map_err(|error| ProductionLeaseError::postgres("load Request workflow", &error))?;
                let Some(row) = row else {
                    return Ok(Ok(None));
                };
                let definition = Definition::from_row(&row)?;
                let permission =
                    wamn_routes::workflow_permission(&app.application, definition.id.as_str());
                if !request.caller.permits(&permission) {
                    return Ok(Err(Refusal::PermissionDenied));
                }
                let (principal, role) =
                    match request_identity(request.caller, definition.role.as_deref()) {
                        Ok(identity) => identity,
                        Err(error) => return Ok(Err(error)),
                    };
                let json = serde_json::from_str(&definition.graph).map_err(|error| {
                    ProductionLeaseError::invalid("decode Request workflow", error)
                })?;
                let graph = wamn_workflow_graph::WorkflowGraph::parse(&json).map_err(|error| {
                    ProductionLeaseError::invalid("parse Request workflow", error)
                })?;
                if graph.graph_hash() != definition.hash {
                    return Err(ProductionLeaseError::contract(
                        "load Request workflow",
                        "graph hash mismatch",
                    ));
                }
                let checked = super::authoring::check(&graph, app, definition.role.as_deref())
                    .map_err(|error| {
                        ProductionLeaseError::invalid("check Request workflow", error)
                    })?;
                let key = uuid::Uuid::new_v4().to_string();
                if checked.can_write {
                    let role = String::from(role);
                    let Admission::Leased(mut admitted) = workload
                        .admit(
                            connection,
                            definition,
                            Delivery {
                                trigger: RunTrigger::Request,
                                invoked_by: "request",
                                principal,
                                role: &role,
                                invoker: Some(request.caller.principal_id()),
                                key: &key,
                                input: &request.input,
                                parent: None,
                            },
                        )
                        .await?
                    else {
                        return Err(ProductionLeaseError::contract(
                            "admit Request workflow",
                            "duplicate request identity",
                        ));
                    };
                    admitted.authority.request_deadline = Some(request.deadline);
                    return Ok(Ok(Some(Prepared::Durable(admitted))));
                }
                let authority = Authority {
                    delivery: key,
                    workflow_id: definition.id.clone(),
                    version: definition.version.unsigned_abs(),
                    hash: definition.hash,
                    principal,
                    role: String::from(role),
                    generation: None,
                    request_deadline: Some(request.deadline),
                    responded: AtomicBool::new(false),
                    causation: None,
                };
                Ok(Ok(Some(Prepared::Inline {
                    authority,
                    definition,
                })))
            });
        let prepared = tokio::time::timeout_at(request.deadline, prepare)
            .await
            .map_err(|_| Refusal::DeadlineElapsed)?
            .map_err(|error| storage_error(&error))??;
        let Some(prepared) = prepared else {
            return Ok(None);
        };
        let (handle, definition, input_json, run_id) = match prepared {
            Prepared::Inline {
                authority,
                definition,
            } => (
                workload.create_authority(authority),
                definition.wit(),
                request.input.to_string(),
                None,
            ),
            Prepared::Durable(admitted) => {
                let run = admitted.issue(&workload);
                (run.authority, run.definition, run.input_json, Some(run.id))
            }
        };
        let (reply, response) = oneshot::channel();
        let durable = run_id.is_some();
        let input = entry::Request {
            handle: handle.clone(),
            definition,
            input_json,
            remaining_ms: remaining_ms(request.deadline),
            run_id,
        };
        let result = tokio::time::timeout_at(request.deadline, async {
            workload
                .entry
                .dispatch(Call {
                    input,
                    reply,
                    deadline: request.deadline,
                })
                .await?;
            Ok::<_, anyhow::Error>(response.await?)
        })
        .await;
        if !durable {
            workload.remove_authority(&handle);
        }
        match result {
            Ok(result) => result.map_err(|error| entry_error(&error))?.map(Some),
            Err(_) => Err(Refusal::DeadlineElapsed),
        }
    }

    /// Admit a durable run chosen by an administrator and start the service walk.
    pub(crate) async fn run_now(&self, request: RunNow<'_>) -> Result<Option<String>, Refusal> {
        if !request.caller.admin() {
            return Err(Refusal::PermissionDenied);
        }
        let workload = self.workload(request.workload)?;
        let app = &workload.application;
        let admitted = self
            .postgres
            .executor_write(&app.project, async |connection| {
                let row = sqlx::query(
                "SELECT workflow_id, version, graph_json::text AS graph, graph_hash, execution_role
                   FROM catalog.workflows WHERE environment = $1 AND application = $2
                    AND enabled AND workflow_id = $3 FOR SHARE"
            ).bind(&app.environment).bind(&app.application).bind(request.workflow_id)
                .fetch_optional(&mut *connection).await
                .map_err(|error| ProductionLeaseError::postgres("load run-now workflow", &error))?;
                let Some(row) = row else {
                    return Ok(Ok(None));
                };
                let definition = Definition::from_row(&row)?;
                let identity = match request.run_as {
                    RunAs::Admin => request_identity(request.caller, None),
                    RunAs::Workflow => definition
                        .role
                        .as_deref()
                        .map(|role| (workflow_user_id(), role))
                        .ok_or_else(|| {
                            Refusal::InvalidInput("workflow has no execution role".into())
                        }),
                };
                let (principal, role) = match identity {
                    Ok(identity) => identity,
                    Err(error) => return Ok(Err(error)),
                };
                let role = String::from(role);
                Ok(Ok(Some(
                    workload
                        .admit(
                            connection,
                            definition,
                            Delivery {
                                trigger: RunTrigger::RunNow,
                                invoked_by: "manual",
                                principal,
                                role: &role,
                                invoker: Some(request.caller.principal_id()),
                                key: request.idempotency_key,
                                input: request.input,
                                parent: None,
                            },
                        )
                        .await?,
                )))
            })
            .await
            .map_err(|error| storage_error(&error))??;
        let Some(admitted) = admitted else {
            return Ok(None);
        };
        match admitted {
            Admission::Existing(id) => Ok(Some(id)),
            Admission::Conflict => Err(Refusal::IdempotencyConflict),
            Admission::Leased(admitted) => {
                let run = admitted.issue(&workload);
                let id = run.id.clone();
                tokio::spawn(async move {
                    if let Err(error) = workload.entry.dispatch(Run { run }).await {
                        tracing::warn!(%error, "dispatch run-now workflow");
                    }
                });
                Ok(Some(id))
            }
        }
    }
}

enum Prepared {
    Inline {
        authority: Authority,
        definition: Definition,
    },
    Durable(Box<super::admission::Admitted>),
}

fn request_identity<'a>(
    caller: &'a AuthenticatedCaller,
    role: Option<&'a str>,
) -> Result<(PrincipalId, &'a str), Refusal> {
    if let Some(role) = role {
        return Ok((workflow_user_id(), role));
    }
    let role = caller.active_role().ok_or(Refusal::ActiveRoleRequired)?;
    Ok((*caller.principal_id(), role))
}

pub(super) fn remaining_ms(deadline: Instant) -> u64 {
    u64::try_from(
        deadline
            .saturating_duration_since(Instant::now())
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn entry_error(error: &anyhow::Error) -> Refusal {
    tracing::warn!(%error, "workflow service dispatch failed");
    Refusal::Unavailable
}

struct Call {
    input: entry::Request,
    reply: oneshot::Sender<Result<entry::Response, Refusal>>,
    deadline: Instant,
}

impl GuestCall for Call {
    fn describe(&self) -> &'static str {
        "wamn:workflow/entry#call"
    }
    fn deadline(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
    fn call(
        self: Box<Self>,
        accessor: &Accessor<SharedCtx>,
        instance: Instance,
    ) -> GuestCallFuture<'_> {
        Box::pin(async move {
            let Self {
                mut input,
                reply,
                deadline,
            } = *self;
            input.remaining_ms = remaining_ms(deadline);
            let entry = accessor.with(|mut access| Service::new(&mut access, &instance))?;
            let result = entry
                .wamn_workflow_entry()
                .call_call(accessor, input)
                .await?;
            let refused = result.is_err().then_some("workflow");
            let _ = reply.send(result);
            Ok(refused)
        })
    }
}

struct Run {
    run: wit_run::LeasedRun,
}

impl GuestCall for Run {
    fn describe(&self) -> &'static str {
        "wamn:workflow/entry#run"
    }
    fn call(
        self: Box<Self>,
        accessor: &Accessor<SharedCtx>,
        instance: Instance,
    ) -> GuestCallFuture<'_> {
        Box::pin(async move {
            let entry = accessor.with(|mut access| Service::new(&mut access, &instance))?;
            let result = entry
                .wamn_workflow_entry()
                .call_run(accessor, self.run)
                .await?;
            if let Err(error) = result {
                tracing::warn!(?error, "workflow service refused an admitted run");
                return Ok(Some("workflow"));
            }
            Ok(None)
        })
    }
}

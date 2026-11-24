//! Author and change the enabled workflow under the project lifecycle lock.

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use std::sync::Arc;
use wamn_application::application_config::{ApplicationConfig, check_authored_workflow};
use wamn_application::ingress::AuthenticatedCaller;
use wamn_application::native_routes::DeliveryError;
use wamn_identity_protocol::PrincipalId;
use wamn_package::canonical_name::CanonicalName;
use wamn_plugin::OperationHost;
use wamn_postgres::{control, workflow};
use wamn_routes::HostAttachment;
use wamn_routes::HostHandler;
use wamn_workflow_graph::{GraphTrigger, WorkflowGraph};

use super::{
    RunAs, RunNow, Workflow, Workload,
    broker::{Broker, Consumer},
};
use crate::host::route_refusal::{Refusal, parse};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Author {
    graph: Value,
    execution_role: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Version {
    workflow_id: CanonicalName,
    version: i32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunRequest {
    workflow_id: CanonicalName,
    run_as: RunAs,
    input_json: Value,
    idempotency_key: CanonicalName,
}

impl Workflow {
    pub(in crate::host) async fn administer(
        &self,
        workload: &str,
        attachment: &HostAttachment,
        caller: &AuthenticatedCaller,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let state = self
            .workload(workload)
            .map_err(|error| entry_refusal(&error))?;
        let app = &state.application;
        if attachment.route.handler == HostHandler::WorkflowRun {
            let run: RunRequest = parse(payload)?;
            let id = self
                .run_now(RunNow {
                    workload,
                    workflow_id: run.workflow_id.as_str(),
                    caller,
                    run_as: run.run_as,
                    input: &run.input_json,
                    idempotency_key: run.idempotency_key.as_str(),
                })
                .await
                .map_err(|error| entry_refusal(&error))?
                .ok_or_else(|| Refusal::Declared {
                    code: "workflow_not_enabled",
                    detail: json!({"field": "workflow_id"}),
                })?;
            return Ok(json!({"run_id": id}));
        }
        let handler = attachment.route.handler;
        let operation = CanonicalName::from(attachment.operation.clone());
        if matches!(
            handler,
            HostHandler::WorkflowEnable | HostHandler::WorkflowDisable
        ) {
            let version = parse(payload)?;
            let broker = Arc::clone(&self.broker);
            let postgres = Arc::clone(&self.postgres);
            let principal = *caller.principal_id();
            // Finish the database and broker change even if the HTTP caller leaves.
            return tokio::spawn(async move {
                change(
                    &postgres, &broker, &state, &principal, &operation, handler, version,
                )
                .await
            })
            .await
            .context("finish workflow lifecycle operation")?;
        }
        self.postgres
            .administration_transaction(
                &app.project,
                caller.principal_id(),
                &operation,
                async |connection| {
                    let result = match handler {
                        HostHandler::WorkflowList => workflow::list(connection, app).await?,
                        HostHandler::WorkflowAuthor => {
                            require_lock(workflow::try_author_lock(connection, app).await?)?;
                            author(connection, app, caller, parse(payload)?).await?
                        }
                        _ => return Err(Refusal::Delivery(DeliveryError::SourceNotFound)),
                    };
                    Ok(json!({"result": result}))
                },
            )
            .await?
    }
}

fn require_lock(held: bool) -> Result<(), Refusal> {
    if held {
        Ok(())
    } else {
        Err(Refusal::Declared {
            code: "apply_in_progress",
            detail: json!({}),
        })
    }
}

enum ConsumerChange {
    Keep,
    DeleteAfterCommit(Consumer),
    DeleteOnRollback(Consumer),
}

async fn change(
    postgres: &OperationHost,
    broker: &Broker,
    workload: &Workload,
    principal: &PrincipalId,
    operation: &CanonicalName,
    handler: HostHandler,
    version: Version,
) -> Result<Value, Refusal> {
    let app = &workload.application;
    let mut session = postgres
        .begin_administration(&app.project, principal, operation)
        .await?;
    let connection = session.connection();
    // Keep the same key through commit and broker settlement so another host
    // cannot enable a replacement while this host deletes its predecessor.
    let outcome = async {
        require_lock(control::try_environment_lock(connection, &app.org, &app.environment).await?)?;
        match handler {
            HostHandler::WorkflowEnable => enable(connection, broker, workload, &version).await,
            HostHandler::WorkflowDisable => disable(connection, broker, workload, &version).await,
            _ => Err(Refusal::Delivery(DeliveryError::SourceNotFound)),
        }
    }
    .await;
    let ended = if outcome.is_ok() {
        workflow::commit_change(connection).await
    } else {
        workflow::rollback_change(connection).await
    };
    let cleanup = match (&outcome, ended.is_ok()) {
        (Ok(ConsumerChange::DeleteAfterCommit(consumer)), true)
        | (Ok(ConsumerChange::DeleteOnRollback(consumer)), false) => consumer.delete().await,
        _ => Ok(()),
    };
    if let Err(error) = ended {
        if let Err(cleanup) = cleanup {
            tracing::warn!(error = %cleanup, workflow_id = %version.workflow_id, "compensate workflow consumer creation");
        }
        return Err(Refusal::Failed(
            anyhow::Error::from(error).context("end the workflow lifecycle transaction"),
        ));
    }
    control::unlock_all_advisory_locks(connection).await?;
    session.repool();
    cleanup?;
    outcome?;
    Ok(json!({"result": version_result(version)}))
}

async fn author(
    connection: &mut PgConnection,
    app: &ApplicationConfig,
    caller: &AuthenticatedCaller,
    request: Author,
) -> Result<Value, Refusal> {
    let graph = WorkflowGraph::parse(&request.graph).map_err(invalid_graph)?;
    let version = i32::try_from(graph.version).map_err(invalid_graph)?;
    let role = request.execution_role.as_deref();
    let checked = check_authored_workflow(app, &graph, role).map_err(invalid_graph)?;
    if let GraphTrigger::Request { path } = checked.entry {
        check_path(connection, app, &graph.graph_id, path).await?;
    }
    let Some(hash) = workflow::insert_version(
        connection,
        app,
        &graph,
        version,
        checked.entry,
        role,
        caller.principal_id(),
    )
    .await?
    else {
        return Err(Refusal::Declared {
            code: "workflow_version_exists",
            detail: json!({}),
        });
    };
    Ok(json!({"workflow_id": graph.graph_id, "version": version, "graph_hash": hash}))
}

async fn check_path(
    connection: &mut PgConnection,
    app: &ApplicationConfig,
    id: &CanonicalName,
    path: &str,
) -> Result<(), Refusal> {
    if workflow::path_taken(connection, app, id, path).await? {
        Err(Refusal::Declared {
            code: "workflow_path_taken",
            detail: json!({"path": path}),
        })
    } else {
        Ok(())
    }
}

async fn enable(
    connection: &mut PgConnection,
    broker: &Broker,
    workload: &Workload,
    version: &Version,
) -> Result<ConsumerChange, Refusal> {
    let app = &workload.application;
    let row = workflow::load_version(connection, app, &version.workflow_id, version.version)
        .await?
        .ok_or_else(unknown_version)?;
    let value = row.graph()?;
    let graph = WorkflowGraph::parse(&value).map_err(invalid_graph)?;
    let hash = row.hash()?;
    if graph.graph_hash() != hash {
        return Err(invalid_graph("workflow graph hash does not match"));
    }
    let role = row.role()?;
    let checked = check_authored_workflow(app, &graph, role).map_err(invalid_graph)?;
    if let GraphTrigger::Request { path } = checked.entry {
        check_path(connection, app, &version.workflow_id, path).await?;
    }
    if row.enabled()? {
        if !matches!(checked.entry, GraphTrigger::DataChange { .. }) {
            // A retry of a trigger change must finish any post-commit deletion.
            if workflow::previously_had_consumer(connection, app, &version.workflow_id).await? {
                return Ok(ConsumerChange::DeleteAfterCommit(
                    broker.consumer(workload, &version.workflow_id).await?,
                ));
            }
        }
        return Ok(ConsumerChange::Keep);
    }
    let data_change = if let GraphTrigger::DataChange { entity, ops, .. } = checked.entry {
        let consumer = broker.consumer(workload, &version.workflow_id).await?;
        let start = consumer
            .last_sequence()
            .await?
            .checked_add(1)
            .context("workflow source stream sequence overflow")?;
        Some((consumer, start, entity, ops))
    } else {
        None
    };
    let had_data_change =
        workflow::enable_version(connection, app, &version.workflow_id, version.version).await?;
    if let Some((consumer, start, entity, ops)) = data_change {
        let already_owned = had_data_change && consumer.exists().await?;
        let filters = ops
            .iter()
            .map(|op| {
                wamn_registry::name::event_subject(
                    &app.org,
                    &app.project,
                    &app.environment,
                    &format!("{entity}.{}", op.as_str()),
                )
            })
            .collect();
        consumer.create(filters, start).await?;
        // A rollback keeps another version's existing consumer.
        return Ok(if already_owned {
            ConsumerChange::Keep
        } else {
            ConsumerChange::DeleteOnRollback(consumer)
        });
    }
    if had_data_change {
        let consumer = broker.consumer(workload, &version.workflow_id).await?;
        return Ok(ConsumerChange::DeleteAfterCommit(consumer));
    }
    Ok(ConsumerChange::Keep)
}

async fn disable(
    connection: &mut PgConnection,
    broker: &Broker,
    workload: &Workload,
    version: &Version,
) -> Result<ConsumerChange, Refusal> {
    let app = &workload.application;
    let trigger = workflow::disable_version(connection, app, &version.workflow_id, version.version)
        .await?
        .ok_or_else(unknown_version)?;
    if trigger != "data_change" {
        return Ok(ConsumerChange::Keep);
    }
    if workflow::has_enabled_data_change(connection, app, &version.workflow_id).await? {
        return Ok(ConsumerChange::Keep);
    }
    Ok(ConsumerChange::DeleteAfterCommit(
        broker.consumer(workload, &version.workflow_id).await?,
    ))
}

fn version_result(version: Version) -> Value {
    json!({"workflow_id": String::from(version.workflow_id), "version": version.version})
}

fn invalid_graph(error: impl std::fmt::Display) -> Refusal {
    Refusal::Declared {
        code: "invalid_input",
        detail: json!({"field": "graph", "message": error.to_string()}),
    }
}

fn unknown_version() -> Refusal {
    Refusal::Declared {
        code: "workflow_version_not_found",
        detail: json!({}),
    }
}

fn entry_refusal(error: &super::Refusal) -> Refusal {
    use super::Refusal as Entry;
    let (code, field) = match error {
        Entry::InvalidInput(_) => ("invalid_input", "run_as"),
        Entry::ActiveRoleRequired => ("active_role_required", "run_as"),
        Entry::IdempotencyConflict => ("idempotency_conflict", "idempotency_key"),
        _ => return Refusal::Delivery(DeliveryError::ExecutionFailed),
    };
    Refusal::Declared {
        code,
        detail: json!({"field": field}),
    }
}

/// Return the grant references of the enabled Request workflows of this application.
pub(in crate::host) async fn permissions(
    connection: &mut PgConnection,
    app: &ApplicationConfig,
) -> Result<Vec<String>, sqlx::Error> {
    let ids = workflow::request_workflow_ids(connection, app).await?;
    Ok(ids
        .iter()
        .map(|id| wamn_routes::workflow_permission(&app.application, id))
        .collect())
}

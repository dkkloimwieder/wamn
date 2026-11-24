//! Admit UTC schedule ticks and verified data changes on the project platform login.

use super::{
    Authority, BoundService, LEASE_MS, LEASE_TTL, Refusal, SharedCtx, WasmResult, Workflow,
    Workload, service, storage_error, wit_run,
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::Instant;
use wamn_event::{Causation, SourceEvent, matching};
use wamn_identity_protocol::PrincipalId;
use wamn_identity_protocol::workflow_user_id;
use wamn_plugin::admission::{RunAdmission, RunAdmitted, RunTrigger};
use wamn_plugin::operation_sql::executor_write;
use wamn_plugin::persistence::{LeaseTurn, ProductionLeaseResult};
use wamn_plugin::run_store::PostgresRunStore;
use wamn_plugin::run_store::{
    ProductionCompletion, ProductionCompletionResult, ProductionLeaseError, ProductionLeaseRenewal,
};
use wamn_plugin::workflow::admission::{self as stored, Definition};
use wash_runtime::wasmtime::component::Accessor;

pub(super) fn wit_definition(definition: Definition) -> wit_run::Workflow {
    wit_run::Workflow {
        id: definition.id.into(),
        version: definition.version.unsigned_abs(),
        graph_json: definition.graph,
    }
}

pub(super) struct Delivery<'a> {
    pub trigger: RunTrigger,
    pub principal: PrincipalId,
    pub role: &'a str,
    pub invoked_by: &'a str,
    pub invoker: Option<&'a PrincipalId>,
    pub key: &'a str,
    pub input: &'a Value,
    pub parent: Option<&'a Causation>,
}

pub(super) struct Admitted {
    pub(super) authority: Authority,
    definition: Definition,
    input_json: String,
    checkpoint_json: Option<String>,
}

impl Admitted {
    pub(super) fn issue(self, workload: &Workload) -> wit_run::LeasedRun {
        let id = self.authority.delivery.clone();
        let authority = workload.create_authority(Arc::new(self.authority));
        wit_run::LeasedRun {
            id,
            authority,
            definition: wit_definition(self.definition),
            input_json: self.input_json,
            checkpoint_json: self.checkpoint_json,
        }
    }
}

pub(super) enum Admission {
    Leased(Box<Admitted>),
    Existing(String),
    Conflict,
}

impl Admission {
    fn winner(self) -> Result<Option<Admitted>, ProductionLeaseError> {
        match self {
            Self::Leased(run) => Ok(Some(*run)),
            Self::Existing(_) => Ok(None),
            Self::Conflict => Err(ProductionLeaseError::contract(
                "admit workflow",
                "idempotency conflict",
            )),
        }
    }
}

impl Workload {
    pub(super) async fn admit(
        &self,
        connection: &mut sqlx::PgConnection,
        definition: Definition,
        delivery: Delivery<'_>,
    ) -> Result<Admission, ProductionLeaseError> {
        let depth = delivery.parent.map(|parent| parent.depth.saturating_add(1));
        let stored_depth = depth
            .map(i32::try_from)
            .transpose()
            .map_err(|error| ProductionLeaseError::invalid("convert workflow depth", error))?;
        let run = PostgresRunStore::new(connection)
            .admit_run(
                &RunAdmission {
                    application: &self.application.application,
                    environment: &self.application.environment,
                    workflow_id: &definition.id,
                    workflow_version: definition.version,
                    graph_hash: &definition.hash,
                    execution_principal_id: &delivery.principal,
                    execution_role: delivery.role,
                    trigger: delivery.trigger,
                    invoked_by: delivery.invoked_by,
                    invoked_by_user_id: delivery.invoker,
                    idempotency_key: delivery.key,
                    input: delivery.input,
                    source_run: delivery.parent.map(|parent| parent.run.as_str()),
                    root_run: delivery.parent.map(|parent| parent.root.as_str()),
                    depth: stored_depth,
                },
                &self.runner,
                LEASE_MS,
            )
            .await?;
        match run {
            RunAdmitted::Existing { run_id } => Ok(Admission::Existing(run_id)),
            RunAdmitted::Conflict => Ok(Admission::Conflict),
            RunAdmitted::Leased { run_id, generation } => {
                let causation = delivery.parent.zip(depth).map(|(parent, depth)| Causation {
                    run: run_id.clone(),
                    root: parent.root.clone(),
                    depth,
                });
                Ok(Admission::Leased(Box::new(Admitted {
                    authority: Authority {
                        delivery: run_id,
                        workflow_id: definition.id.clone(),
                        version: definition.version.unsigned_abs(),
                        hash: definition.hash,
                        principal: delivery.principal,
                        role: String::from(delivery.role),
                        generation: Some(generation),
                        request_deadline: None,
                        responded: AtomicBool::new(false),
                        causation,
                    },
                    definition,
                    input_json: delivery.input.to_string(),
                    checkpoint_json: None,
                })))
            }
        }
    }
}

impl Workflow {
    async fn schedules(
        &self,
        workload: &Workload,
        due: DateTime<Utc>,
    ) -> Result<Vec<wit_run::LeasedRun>, Refusal> {
        let key = due.to_rfc3339_opts(SecondsFormat::Secs, true);
        let input = serde_json::json!({ "due": &key });
        let app = &workload.application;
        let admitted = executor_write(&self.database, &app.project, async |connection| {
            let rows = stored::schedules(connection, app).await?;
            let mut admitted = Vec::new();
            for row in rows {
                let schedule = row.schedule()?;
                if !wamn_workflow_graph::schedule::is_due(schedule, due.naive_utc()).map_err(
                    |error| ProductionLeaseError::invalid("match workflow schedule", error),
                )? {
                    continue;
                }
                let definition = row.definition()?;
                let role = row.role()?;
                if let Some(run) = workload
                    .admit(
                        connection,
                        definition,
                        Delivery {
                            trigger: RunTrigger::Schedule,
                            invoked_by: "schedule",
                            principal: workflow_user_id(),
                            role,
                            invoker: None,
                            key: &key,
                            input: &input,
                            parent: None,
                        },
                    )
                    .await?
                    .winner()?
                {
                    admitted.push(run);
                }
            }
            Ok(admitted)
        })
        .await
        .map_err(|error| storage_error(&error))?;
        Ok(admitted
            .into_iter()
            .map(|run| run.issue(workload))
            .collect())
    }

    async fn consumers(
        &self,
        workload: &Workload,
    ) -> Result<Vec<wit_run::DataChangeConsumer>, Refusal> {
        let app = &workload.application;
        let workflows = self
            .database
            .executor_read(&app.project, async |connection| {
                Ok(stored::consumer_workflows(connection, app).await?)
            })
            .await
            .map_err(|error| {
                tracing::warn!(%error, "read enabled data-change consumers");
                Refusal::Unavailable
            })?;
        let stream = wamn_event::stream_name(&app.org, &app.project, &app.environment);
        Ok(workflows
            .into_iter()
            .map(|id| wit_run::DataChangeConsumer {
                consumer: wamn_registry::resource_names::workflow_event_consumer(
                    &app.application,
                    &id,
                ),
                workflow_id: id.into(),
                stream: stream.clone(),
            })
            .collect())
    }

    async fn data_change(
        &self,
        workload: &Workload,
        message: wit_run::DataChange,
    ) -> Result<Option<wit_run::LeasedRun>, Refusal> {
        let app = &workload.application;
        let invalid = |message: &str| Refusal::InvalidInput(message.into());
        if message.stream_sequence == 0 {
            return Err(invalid("invalid stream sequence"));
        }
        let SourceEvent::Cdc(envelope) = SourceEvent::decode(&message.body)
            .map_err(|_| invalid("invalid data-change envelope"))?
        else {
            return Ok(None);
        };
        let headers: Vec<_> = message.message_ids.iter().map(String::as_str).collect();
        let source_id = wamn_event::verified_source_event_id(
            &app.project,
            &app.environment,
            &envelope,
            &headers,
        )
        .ok_or_else(|| invalid("invalid Nats-Msg-Id"))?;
        if wamn_event::subject(
            &app.org,
            &app.project,
            &app.environment,
            &envelope.entity,
            envelope.op,
        ) != message.subject
        {
            return Err(invalid("data-change subject disagrees with the envelope"));
        }
        let result = executor_write(&self.database, &app.project, async |connection| {
            let row = stored::data_change(connection, app, &message.workflow_id, &envelope).await?;
            let Some(row) = row else { return Ok(None) };
            let condition = row.condition()?;
            if !matching::matches(&envelope, condition, &workload.project_key)
                .map_err(event_error)?
            {
                return Ok(None);
            }
            let actor = PostgresRunStore::new(connection)
                .history_actor(&envelope)
                .await?;
            let definition = row.definition()?;
            let role = row.role()?;
            workload
                .admit(
                    connection,
                    definition,
                    Delivery {
                        trigger: RunTrigger::DataChange,
                        invoked_by: &format!("event:{}", source_id.as_str()),
                        principal: workflow_user_id(),
                        role,
                        invoker: actor.as_ref(),
                        key: source_id.as_str(),
                        input: &matching::event_input(&envelope),
                        parent: envelope.causation.as_ref(),
                    },
                )
                .await
                .and_then(Admission::winner)
        })
        .await
        .map_err(|error| storage_error(&error))?;
        Ok(result.map(|run| run.issue(workload)))
    }

    async fn take(&self, workload: &Workload) -> Result<Option<wit_run::LeasedRun>, Refusal> {
        let app = &workload.application;
        let result = executor_write(&self.database, &app.project, async |connection| {
            let mut runs = PostgresRunStore::new(connection);
            runs.reap_exhausted(&app.application, &app.environment, 0)
                .await?;
            let turn = runs
                .lease_next(
                    &workload.runner,
                    &app.application,
                    &app.environment,
                    LEASE_MS,
                )
                .await?;
            let outcome = match turn {
                LeaseTurn::GrantRefused(error) => return Ok(Err(error)),
                LeaseTurn::Leased(ProductionLeaseResult::Empty) => return Ok(Ok(None)),
                LeaseTurn::Leased(outcome) => outcome,
            };
            let ProductionLeaseResult::Ready {
                run_id,
                input_json,
                checkpoint_json,
                lease_generation,
                graph_id,
                graph_version,
                workflow,
            } = outcome
            else {
                return Ok(Ok(None));
            };
            let graph = stored::leased_graph(
                connection,
                app,
                &graph_id,
                graph_version,
                &workflow.graph_hash,
            )
            .await?;
            Ok(Ok(Some(Admitted {
                definition: Definition {
                    id: graph_id.clone(),
                    version: graph_version,
                    graph,
                    hash: workflow.graph_hash,
                    role: None,
                },
                authority: Authority {
                    delivery: run_id,
                    workflow_id: graph_id,
                    version: graph_version.unsigned_abs(),
                    hash: workflow.graph_hash,
                    principal: workflow.execution_principal_id,
                    role: workflow.execution_role,
                    generation: Some(lease_generation),
                    request_deadline: None,
                    responded: AtomicBool::new(false),
                    causation: workflow.causation,
                },
                input_json,
                checkpoint_json,
            })))
        })
        .await
        .map_err(|error| storage_error(&error))?
        .map_err(|error| storage_error(&error))?;
        Ok(result.map(|run| run.issue(workload)))
    }
}

fn event_error(error: matching::MatchError) -> ProductionLeaseError {
    let operation = error.operation();
    if std::error::Error::source(&error).is_some() {
        ProductionLeaseError::invalid(operation, error)
    } else {
        ProductionLeaseError::contract(operation, error.to_string())
    }
}

fn completion(outcome: wit_run::Completion) -> Result<ProductionCompletion, serde_json::Error> {
    use wamn_plugin::FailType;
    use wit_run::FailureType;
    Ok(match outcome {
        wit_run::Completion::Completed(result) => {
            ProductionCompletion::completed(serde_json::from_str(&result)?)
        }
        wit_run::Completion::Failed(failure) => {
            let failure_type = match failure.failure_type {
                FailureType::Terminal => FailType::Terminal,
                FailureType::RetryExhausted => FailType::RetryExhausted,
                FailureType::InvalidInput => FailType::InvalidInput,
                FailureType::RunawayBudget => FailType::RunawayBudget,
                FailureType::UnresolvableName => FailType::UnresolvableName,
                FailureType::HashInvalidBytes => FailType::HashInvalidBytes,
                FailureType::ForeignRevision => FailType::ForeignRevision,
                FailureType::IncompatibleContract => FailType::IncompatibleContract,
            };
            ProductionCompletion::failed(serde_json::from_str(&failure.result_json)?, failure_type)
        }
    })
}

impl wit_run::Host for wash_runtime::engine::ctx::ActiveCtx<'_> {}

impl<T: Send + 'static> wit_run::HostWithStore<T> for SharedCtx {
    async fn admit_schedules(
        accessor: &Accessor<T, Self>,
        unix_seconds: i64,
    ) -> WasmResult<Result<Vec<wit_run::LeasedRun>, Refusal>> {
        let Some(due) = unix_seconds
            .checked_sub(unix_seconds.rem_euclid(60))
            .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
        else {
            return Ok(Err(Refusal::InvalidInput("invalid schedule time".into())));
        };
        let (plugin, workload) = match service(accessor)? {
            Ok(service) => service,
            Err(error) => return Ok(Err(error)),
        };
        Ok(plugin.schedules(&workload, due).await)
    }

    async fn data_change_consumers(
        accessor: &Accessor<T, Self>,
    ) -> WasmResult<Result<Vec<wit_run::DataChangeConsumer>, Refusal>> {
        let (plugin, workload) = match service(accessor)? {
            Ok(service) => service,
            Err(error) => return Ok(Err(error)),
        };
        Ok(plugin.consumers(&workload).await)
    }

    async fn admit_data_change(
        accessor: &Accessor<T, Self>,
        message: wit_run::DataChange,
    ) -> WasmResult<Result<Option<wit_run::LeasedRun>, Refusal>> {
        let (plugin, workload) = match service(accessor)? {
            Ok(service) => service,
            Err(error) => return Ok(Err(error)),
        };
        Ok(plugin.data_change(&workload, message).await)
    }

    async fn take_expired(
        accessor: &Accessor<T, Self>,
    ) -> WasmResult<Result<Option<wit_run::LeasedRun>, Refusal>> {
        let (plugin, workload) = match service(accessor)? {
            Ok(service) => service,
            Err(error) => return Ok(Err(error)),
        };
        Ok(plugin.take(&workload).await)
    }

    async fn renew(
        accessor: &Accessor<T, Self>,
        handle: String,
    ) -> WasmResult<Result<(), Refusal>> {
        let service = service(accessor)?;
        Ok(async {
            let (plugin, workload, authority, generation) = lease(service, &handle)?;
            let outcome = executor_write(
                &plugin.database,
                &workload.application.project,
                async |connection| {
                    PostgresRunStore::new(connection)
                        .renew(&authority.delivery, &workload.runner, generation, LEASE_MS)
                        .await
                },
            )
            .await
            .map_err(|error| storage_error(&error))?;
            if outcome != ProductionLeaseRenewal::Renewed {
                workload.remove_authority(&handle);
                return Err(Refusal::FenceLost);
            }
            if let Some(entry) = workload
                .authorities
                .lock()
                .expect("workflow authority lock poisoned")
                .get_mut(&handle)
            {
                entry.expires = Instant::now() + LEASE_TTL;
            }
            Ok(())
        }
        .await)
    }

    async fn checkpoint(
        accessor: &Accessor<T, Self>,
        handle: String,
        state_json: String,
    ) -> WasmResult<Result<(), Refusal>> {
        let service = service(accessor)?;
        Ok(async {
            let (plugin, workload, authority, generation) = lease(service, &handle)?;
            let state = serde_json::from_str(&state_json)
                .map_err(|_| Refusal::InvalidInput("invalid checkpoint JSON".into()))?;
            let stored = executor_write(
                &plugin.database,
                &workload.application.project,
                async |connection| {
                    PostgresRunStore::new(connection)
                        .checkpoint(&authority.delivery, &workload.runner, generation, &state)
                        .await
                },
            )
            .await
            .map_err(|error| storage_error(&error))?;
            if stored {
                authority.responded.store(true, Ordering::Release);
                Ok(())
            } else {
                Err(Refusal::FenceLost)
            }
        }
        .await)
    }

    async fn finish(
        accessor: &Accessor<T, Self>,
        handle: String,
        outcome: wit_run::Completion,
    ) -> WasmResult<Result<(), Refusal>> {
        let service = service(accessor)?;
        Ok(async {
            let (plugin, workload, authority, generation) = lease(service, &handle)?;
            let outcome = completion(outcome)
                .map_err(|_| Refusal::InvalidInput("invalid result JSON".into()))?;
            let finished = executor_write(
                &plugin.database,
                &workload.application.project,
                async |connection| {
                    PostgresRunStore::new(connection)
                        .complete(&authority.delivery, &workload.runner, generation, &outcome)
                        .await
                },
            )
            .await
            .map_err(|error| storage_error(&error))?;
            workload.remove_authority(&handle);
            match finished {
                ProductionCompletionResult::Terminalized
                | ProductionCompletionResult::AlreadyTerminal(_) => Ok(()),
                ProductionCompletionResult::FenceLost | ProductionCompletionResult::NotFound => {
                    Err(Refusal::FenceLost)
                }
            }
        }
        .await)
    }
}

type LeasedAuthority = (Arc<Workflow>, Arc<Workload>, Arc<Authority>, i64);

fn lease(service: BoundService, handle: &str) -> Result<LeasedAuthority, Refusal> {
    let (plugin, workload) = service?;
    let authority = workload.authority(handle)?;
    let generation = authority.generation.ok_or(Refusal::InvalidAuthority)?;
    Ok((plugin, workload, authority, generation))
}

//! Admit UTC schedule ticks and verified data changes on the project platform login.

use super::{
    Authority, BoundService, LEASE_MS, LEASE_TTL, Refusal, SharedCtx, WasmResult, Workflow,
    Workload, service, storage_error, wit_run,
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use sqlx::Row as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::Instant;
use wamn_catalog::canonical_name::CanonicalName;
use wamn_event::{Causation, SourceEvent};
use wamn_identity_protocol::PrincipalId;
use wamn_identity_protocol::workflow_user_id;
use wamn_run_state::admission::{RunAdmission, RunAdmitted, RunTrigger, admit_run, history_actor};
use wamn_run_state::persistence::{self, LeaseTurn, ProductionLeaseResult};
use wamn_run_state::run_store::{
    ProductionCompletion, ProductionCompletionResult, ProductionLeaseError, ProductionLeaseRenewal,
};
use wash_runtime::wasmtime::component::Accessor;

pub(super) struct Definition {
    pub id: CanonicalName,
    pub version: i32,
    pub graph: String,
    pub hash: wamn_digest::Digest,
    pub role: Option<String>,
}

impl Definition {
    pub(super) fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, ProductionLeaseError> {
        let error = |error| ProductionLeaseError::storage("read workflow version", error);
        Ok(Self {
            id: row.try_get("workflow_id").map_err(error)?,
            version: row.try_get("version").map_err(error)?,
            graph: row.try_get("graph").map_err(error)?,
            hash: row.try_get("wiring_hash").map_err(error)?,
            role: row.try_get("execution_role").map_err(error)?,
        })
    }

    pub(super) fn wit(self) -> wit_run::Workflow {
        wit_run::Workflow {
            id: self.id.into(),
            version: self.version.unsigned_abs(),
            graph_json: self.graph,
        }
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
        let authority = workload.create_authority(self.authority);
        wit_run::LeasedRun {
            id,
            authority,
            definition: self.definition.wit(),
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
        let run = admit_run(
            connection,
            &RunAdmission {
                application: &self.application.application,
                environment: &self.application.environment,
                workflow_id: &definition.id,
                workflow_version: definition.version,
                wiring_hash: &definition.hash,
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
        let admitted = self
            .postgres
            .executor_write(&app.project, async |connection| {
                let rows = sqlx::query(
                    "SELECT workflow_id, version, graph_json::text AS graph, wiring_hash,
                        execution_role, schedule
                   FROM catalog.workflows
                  WHERE environment = $1 AND application = $2 AND enabled AND trigger = 'schedule'
                  ORDER BY workflow_id FOR SHARE",
                )
                .bind(&app.environment)
                .bind(&app.application)
                .fetch_all(&mut *connection)
                .await
                .map_err(|error| {
                    ProductionLeaseError::postgres("read enabled schedules", &error)
                })?;
                let mut admitted = Vec::new();
                for row in rows {
                    let schedule: &str = row
                        .try_get("schedule")
                        .map_err(|error| ProductionLeaseError::storage("read schedule", error))?;
                    if !super::schedule::is_due(schedule, due.naive_utc()).map_err(|error| {
                        ProductionLeaseError::invalid("match workflow schedule", error)
                    })? {
                        continue;
                    }
                    let definition = Definition::from_row(&row)?;
                    let role: &str = row.try_get("execution_role").map_err(|error| {
                        ProductionLeaseError::storage("read schedule role", error)
                    })?;
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
        let workflows = self.postgres.executor_read(&app.project, async |connection| {
            Ok(sqlx::query_scalar::<_, CanonicalName>(
                "SELECT workflow_id FROM catalog.workflows
                  WHERE environment = $1 AND application = $2 AND enabled AND trigger = 'data_change'
                  ORDER BY workflow_id"
            ).bind(&app.environment).bind(&app.application).fetch_all(connection).await?)
        }).await.map_err(|error| {
            tracing::warn!(%error, "read enabled data-change consumers");
            Refusal::Unavailable
        })?;
        let stream = wamn_event::stream_name(&app.org, &app.project, &app.environment);
        Ok(workflows
            .into_iter()
            .map(|id| wit_run::DataChangeConsumer {
                consumer: wamn_catalog::resource_names::workflow_event_consumer(
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
        let result = self.postgres.executor_write(&app.project, async |connection| {
            let row = sqlx::query(
                "SELECT workflow_id, version, graph_json::text AS graph, wiring_hash, execution_role, condition
                   FROM catalog.workflows
                  WHERE environment = $1 AND application = $2 AND workflow_id = $3
                    AND enabled AND trigger = 'data_change' AND source_package = $4
                    AND entity = $5 AND $6 = ANY(ops) AND enable_lsn <= $7::text::pg_lsn
                    FOR SHARE"
            )
            .bind(&app.environment)
            .bind(&app.application)
            .bind(&message.workflow_id)
            .bind(&envelope.package_id)
            .bind(&envelope.entity)
            .bind(envelope.op.as_str())
            // The ID was checked against the envelope's complete source coordinate.
            .bind(wal_lsn(envelope.identity.commit_lsn))
            .fetch_optional(&mut *connection).await
            .map_err(|error| ProductionLeaseError::postgres("match data-change workflow", &error))?;
            let Some(row) = row else { return Ok(None) };
            let condition: Option<&str> = row.try_get("condition")
                .map_err(|error| ProductionLeaseError::storage("read data-change condition", error))?;
            if !super::events::matches(&envelope, condition, &workload.project_key)? {
                return Ok(None);
            }
            let actor = history_actor(connection, &envelope).await?;
            let definition = Definition::from_row(&row)?;
            let role: &str = row.try_get("execution_role")
                .map_err(|error| ProductionLeaseError::storage("read data-change role", error))?;
            workload.admit(connection, definition, Delivery {
                trigger: RunTrigger::DataChange,
                invoked_by: &format!("event:{}", source_id.as_str()),
                principal: workflow_user_id(),
                role,
                invoker: actor.as_ref(),
                key: source_id.as_str(),
                input: &super::events::event_input(&envelope),
                parent: envelope.causation.as_ref(),
            }).await.and_then(Admission::winner)
        }).await.map_err(|error| storage_error(&error))?;
        Ok(result.map(|run| run.issue(workload)))
    }

    async fn take(&self, workload: &Workload) -> Result<Option<wit_run::LeasedRun>, Refusal> {
        let app = &workload.application;
        let result = self
            .postgres
            .executor_write(&app.project, async |connection| {
                persistence::reap_exhausted(connection, &app.application, &app.environment, 0)
                    .await?;
                let turn = persistence::lease_next(
                    connection,
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
                    wiring_id,
                    wiring_version,
                    workflow,
                } = outcome
                else {
                    return Ok(Ok(None));
                };
                let graph = sqlx::query_scalar(
                    "SELECT graph_json::text FROM catalog.workflows
                  WHERE environment = $1 AND application = $2 AND workflow_id = $3
                    AND version = $4 AND wiring_hash = $5",
                )
                .bind(&app.environment)
                .bind(&app.application)
                .bind(&wiring_id)
                .bind(wiring_version)
                .bind(workflow.wiring_hash)
                .fetch_one(connection)
                .await
                .map_err(|error| {
                    ProductionLeaseError::postgres("read the leased workflow version", &error)
                })?;
                Ok(Ok(Some(Admitted {
                    definition: Definition {
                        id: wiring_id.clone(),
                        version: wiring_version,
                        graph,
                        hash: workflow.wiring_hash,
                        role: None,
                    },
                    authority: Authority {
                        delivery: run_id,
                        workflow_id: wiring_id,
                        version: wiring_version.unsigned_abs(),
                        hash: workflow.wiring_hash,
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

fn wal_lsn(lsn: u64) -> String {
    format!("{:X}/{:08X}", lsn >> 32, lsn & u64::from(u32::MAX))
}

fn completion(outcome: wit_run::Completion) -> Result<ProductionCompletion, serde_json::Error> {
    use wamn_run_state::FailType;
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
            let outcome = plugin
                .postgres
                .executor_write(&workload.application.project, async |connection| {
                    persistence::renew(
                        connection,
                        &authority.delivery,
                        &workload.runner,
                        generation,
                        LEASE_MS,
                    )
                    .await
                })
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
            let stored = plugin
                .postgres
                .executor_write(&workload.application.project, async |connection| {
                    persistence::checkpoint(
                        connection,
                        &authority.delivery,
                        &workload.runner,
                        generation,
                        &state,
                    )
                    .await
                })
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
            let finished = plugin
                .postgres
                .executor_write(&workload.application.project, async |connection| {
                    persistence::complete(
                        connection,
                        &authority.delivery,
                        &workload.runner,
                        generation,
                        &outcome,
                    )
                    .await
                })
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

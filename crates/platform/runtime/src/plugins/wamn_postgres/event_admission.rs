//! Host admission of an event-started workflow run.
//!
//! A workflow registration's event enters the queue under the authority of
//! the release that declares the workflow, not a caller's (owner ruling on
//! `wamn-upl3.6`). The host already holds that release, so it admits the run
//! with the executor credential: one `runs` row of the event grain and one
//! `run_queue` row, in one transaction. The executor's column-exact INSERT
//! reaches no service principal, binding world, or legacy flow column, so the
//! run-plane grain check admits event and schedule runs from these columns.
//!
//! A manual workflow run (docs/plan/workflows.md R3, epic 1) enters the queue
//! the same way: the host admits it on `workflow.start` against the release it
//! serves, with the same credential, in one transaction.
//!
//! A schedule workflow run (docs/plan/workflows.md R3, epic 2) is admitted by
//! the host at a due moment, with the same credential, in one transaction
//! that also reads the enabled version and gates it against the release.

use chrono::{DateTime, SecondsFormat, Utc};
use wamn_catalog::{ServingManifest, gate_workflow_document, resolve_workflow_components};
use wamn_project_state::{ADMIN_ROLE, PlatformComponent, workflow_user_id};
use wamn_run_state::queue::{
    insert_event_run_sql, insert_manual_run_sql, insert_run_queue_sql,
    insert_workflow_schedule_run_sql, select_event_run_sql, select_manual_run_sql,
    select_workflow_schedule_run_sql,
};

use super::WamnPostgres;
use super::production_claim::{
    ProductionClaimError, ProductionClaimErrorType, finish_queue_transaction,
    require_executor_authority, storage,
};
use super::workflow_resolution::{PINNED_RELEASE_SQL, parse_graph};
use crate::plugins::wamn_postgres::AuthorityClass;

/// One event run to admit: the released wiring, the registration whose event
/// started it, and the delivery that carried the event.
#[derive(Debug, Clone, Copy)]
pub struct EventRunAdmission<'a> {
    pub package_id: &'a str,
    /// The release that declares the workflow, as its manifest digest.
    pub manifest_digest: &'a str,
    pub environment: &'a str,
    pub wiring_id: &'a str,
    pub wiring_version: i32,
    pub wiring_hash: &'a str,
    /// The qualified registration id, `<package>::<registration>`.
    pub registration_id: &'a str,
    /// The delivery id: a redelivered event admits no second run.
    pub idempotency_key: &'a str,
    pub input: &'a serde_json::Value,
}

/// What an admission did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventRunAdmitted {
    /// The run is queued, newly or by an earlier delivery of the same event.
    Queued { run_id: String },
    /// The delivery id belongs to a different run.
    Conflict,
}

/// The durability class of the environment's policy, read without a lock:
/// the executor holds no UPDATE on the policy table.
const SELECT_DURABILITY_SQL: &str = "SELECT durability_class FROM environment_policies \
     WHERE tenant_id = $1 AND expected_environment = $2";

impl WamnPostgres {
    /// Admit one event run for the tenant, project, and schema bound to
    /// `component_id` (the queue's claim scope).
    pub async fn admit_event_run(
        &self,
        component_id: &str,
        admission: &EventRunAdmission<'_>,
    ) -> Result<EventRunAdmitted, ProductionClaimError> {
        let tenant = self.tenant_for(component_id).ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorType::Identity,
                "resolve event admission tenant",
                "component has no host-injected tenant",
            )
        })?;
        let project = self.project_for(component_id);
        let schema = self.schema_for(component_id);
        let runner = self.runner_for(component_id);
        let user_id = self.user_id_for(component_id);
        let operation = self.operation_for(component_id);
        let application = self.application_for(component_id);
        let (connection, policy) = self
            .checkout_platform(&project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| {
                ProductionClaimError::new(
                    ProductionClaimErrorType::Storage,
                    "checkout event admission connection",
                    format!("{error:?}"),
                )
            })?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                false,
                &tenant,
                schema.as_deref(),
                runner.as_deref(),
                None,
                user_id.as_deref(),
                operation.as_deref(),
                application.as_deref(),
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(ProductionClaimError::new(
                ProductionClaimErrorType::Storage,
                "begin event admission transaction",
                format!("{error:?}"),
            ));
        }
        let result = async {
            require_executor_authority(&connection).await?;
            let durability: String = connection
                .query_opt(SELECT_DURABILITY_SQL, &[&tenant, &admission.environment])
                .await
                .map_err(|error| storage("read the environment policy", &error))?
                .ok_or_else(|| {
                    ProductionClaimError::new(
                        ProductionClaimErrorType::Contract,
                        "read the environment policy",
                        "the environment policy is absent",
                    )
                })?
                .get(0);
            let input = admission.input.to_string();
            let binds: [&(dyn tokio_postgres::types::ToSql + Sync); 10] = [
                &tenant,
                &admission.package_id,
                &admission.manifest_digest,
                &admission.environment,
                &admission.wiring_id,
                &admission.wiring_version,
                &admission.wiring_hash,
                &admission.registration_id,
                &admission.idempotency_key,
                &input,
            ];
            let mut insert = binds.to_vec();
            insert.push(&durability);
            let inserted = connection
                .query_opt(&insert_event_run_sql(), &insert)
                .await
                .map_err(|error| storage("admit the event run", &error))?;
            if let Some(row) = inserted {
                let run_id: String = row.get(0);
                connection
                    .execute(insert_run_queue_sql(), &[&tenant, &run_id])
                    .await
                    .map_err(|error| storage("queue the event run", &error))?;
                return Ok(EventRunAdmitted::Queued { run_id });
            }
            Ok(
                match connection
                    .query_opt(select_event_run_sql(), &binds)
                    .await
                    .map_err(|error| storage("read the first event run", &error))?
                {
                    Some(row) => EventRunAdmitted::Queued { run_id: row.get(0) },
                    None => EventRunAdmitted::Conflict,
                },
            )
        }
        .await;
        finish_queue_transaction(self, connection, result, "commit the event admission").await
    }
}

/// One manual workflow run to admit: the enabled workflow version, the
/// release it gated against, who it executes as, who started it, and the
/// component digest of every node.
#[derive(Debug, Clone, Copy)]
pub struct ManualRunAdmission<'a> {
    /// The application package of the release.
    pub package_id: &'a str,
    /// The release the host serves, as its manifest digest.
    pub manifest_digest: &'a str,
    pub environment: &'a str,
    pub workflow_id: &'a str,
    pub workflow_version: i32,
    pub wiring_hash: &'a str,
    pub execution_principal_id: &'a str,
    pub execution_role: &'a str,
    /// The session or PAT user that started the run.
    pub invoked_by_user_id: &'a str,
    /// `{node id: component digest}`.
    pub node_digests: &'a serde_json::Value,
    /// A repeated key with the same input admits no second run.
    pub idempotency_key: &'a str,
    pub input: &'a serde_json::Value,
}

/// What a manual admission did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManualRunAdmitted {
    /// The run is queued, newly or by an earlier start with the same key and
    /// input.
    Queued { run_id: String },
    /// The key belongs to a run with another input.
    Conflict,
}

impl WamnPostgres {
    /// Admit one manual workflow run in `project` for `tenant`: one `runs`
    /// row of the manual grain and one `run_queue` row, in one transaction
    /// under the executor credential.
    pub async fn admit_manual_run(
        &self,
        project: &str,
        tenant: &str,
        admission: &ManualRunAdmission<'_>,
    ) -> Result<ManualRunAdmitted, ProductionClaimError> {
        let (connection, policy) = self
            .checkout_platform(project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| {
                ProductionClaimError::new(
                    ProductionClaimErrorType::Storage,
                    "checkout manual admission connection",
                    format!("{error:?}"),
                )
            })?;
        let executor = PlatformComponent::Executor.principal_id().to_string();
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                false,
                tenant,
                Some("wamn_run"),
                None,
                None,
                Some(&executor),
                Some(PlatformComponent::Executor.principal_name()),
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(ProductionClaimError::new(
                ProductionClaimErrorType::Storage,
                "begin manual admission transaction",
                format!("{error:?}"),
            ));
        }
        let result = async {
            require_executor_authority(&connection).await?;
            let durability: String = connection
                .query_opt(SELECT_DURABILITY_SQL, &[&tenant, &admission.environment])
                .await
                .map_err(|error| storage("read the environment policy", &error))?
                .ok_or_else(|| {
                    ProductionClaimError::new(
                        ProductionClaimErrorType::Contract,
                        "read the environment policy",
                        "the environment policy is absent",
                    )
                })?
                .get(0);
            let input = admission.input.to_string();
            let node_digests = admission.node_digests.to_string();
            let binds: [&(dyn tokio_postgres::types::ToSql + Sync); 14] = [
                &tenant,
                &admission.package_id,
                &admission.manifest_digest,
                &admission.environment,
                &admission.workflow_id,
                &admission.workflow_version,
                &admission.wiring_hash,
                &admission.execution_principal_id,
                &admission.execution_role,
                &admission.invoked_by_user_id,
                &node_digests,
                &admission.idempotency_key,
                &input,
                &durability,
            ];
            let inserted = connection
                .query_opt(&insert_manual_run_sql(), &binds)
                .await
                .map_err(|error| storage("admit the manual run", &error))?;
            if let Some(row) = inserted {
                let run_id: String = row.get(0);
                connection
                    .execute(insert_run_queue_sql(), &[&tenant, &run_id])
                    .await
                    .map_err(|error| storage("queue the manual run", &error))?;
                return Ok(ManualRunAdmitted::Queued { run_id });
            }
            Ok(
                match connection
                    .query_opt(
                        select_manual_run_sql(),
                        &[&tenant, &admission.idempotency_key, &input],
                    )
                    .await
                    .map_err(|error| storage("read the first manual run", &error))?
                {
                    Some(row) => ManualRunAdmitted::Queued { run_id: row.get(0) },
                    None => ManualRunAdmitted::Conflict,
                },
            )
        }
        .await;
        finish_queue_transaction(self, connection, result, "commit the manual admission").await
    }
}

/// The enabled version of one schedule workflow. Binds: `$1` tenant, `$2`
/// environment, `$3` application, `$4` workflow id.
const ENABLED_SCHEDULE_SQL: &str = "\
SELECT version, graph_json::text, wiring_hash, execution_role \
  FROM catalog.workflows \
 WHERE tenant_id = $1 AND environment = $2 AND application = $3 \
   AND workflow_id = $4 AND enabled AND trigger = 'schedule'";

/// One due moment of a schedule workflow to admit, against the release the
/// host serves.
#[derive(Debug, Clone, Copy)]
pub struct ScheduleRunAdmission<'a> {
    pub environment: &'a str,
    /// The application package of the release.
    pub application: &'a str,
    pub workflow_id: &'a str,
    /// The release the host serves, as its manifest digest.
    pub manifest_digest: &'a str,
    pub due: DateTime<Utc>,
}

/// What a schedule admission did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleRunAdmitted {
    /// The run is queued, newly or by an earlier admission of the same due
    /// moment.
    Queued { run_id: String },
    /// No version of the workflow with a schedule trigger is enabled. No row
    /// is written.
    NotEnabled,
    /// The enabled version does not gate against the release, or its graph
    /// does not hash to its stored hash. No row is written.
    NotServed,
    /// The key belongs to a run of another grain or input.
    Conflict,
}

/// The canonical UTC spelling of a due moment, as the schedule task spells
/// it in its keys and input.
fn due_text(due: DateTime<Utc>) -> String {
    due.to_rfc3339_opts(SecondsFormat::Secs, true)
}

impl WamnPostgres {
    /// Admit the run of schedule workflow `workflow_id` due at `due`, in
    /// `project` for `tenant`: in one transaction under the executor
    /// credential, read the enabled schedule version, gate its graph against
    /// the release `manifest_digest`, freeze the component digest of every
    /// node, and write one `runs` row of the schedule workflow grain and one
    /// `run_queue` row. The run executes as the `workflow` user with the
    /// execution role, or with `admin` for `invoker`. The key
    /// `schedule:{application}/{workflow_id}:{due}` makes a second admission
    /// of the same due moment answer the first run.
    pub async fn admit_schedule_run(
        &self,
        project: &str,
        tenant: &str,
        admission: &ScheduleRunAdmission<'_>,
    ) -> Result<ScheduleRunAdmitted, ProductionClaimError> {
        let (connection, policy) = self
            .checkout_platform(project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| {
                ProductionClaimError::new(
                    ProductionClaimErrorType::Storage,
                    "checkout schedule admission connection",
                    format!("{error:?}"),
                )
            })?;
        let executor = PlatformComponent::Executor.principal_id().to_string();
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                false,
                tenant,
                Some("wamn_run"),
                None,
                None,
                Some(&executor),
                Some(PlatformComponent::Executor.principal_name()),
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(ProductionClaimError::new(
                ProductionClaimErrorType::Storage,
                "begin schedule admission transaction",
                format!("{error:?}"),
            ));
        }
        let result = async {
            require_executor_authority(&connection).await?;
            let Some(workflow) = connection
                .query_opt(
                    ENABLED_SCHEDULE_SQL,
                    &[
                        &tenant,
                        &admission.environment,
                        &admission.application,
                        &admission.workflow_id,
                    ],
                )
                .await
                .map_err(|error| storage("read the enabled schedule version", &error))?
            else {
                return Ok(ScheduleRunAdmitted::NotEnabled);
            };
            let version: i32 = workflow.get(0);
            let graph_json: String = workflow.get(1);
            let wiring_hash: String = workflow.get(2);
            let execution_role: Option<String> = workflow.get(3);
            let contract = |operation: &'static str, detail: String| {
                ProductionClaimError::new(ProductionClaimErrorType::Contract, operation, detail)
            };
            let graph = parse_graph(&graph_json).map_err(|error| {
                contract("parse the stored workflow graph", format!("{error:#}"))
            })?;
            let bytes: Vec<u8> = connection
                .query_opt(PINNED_RELEASE_SQL, &[&tenant, &admission.manifest_digest])
                .await
                .map_err(|error| storage("read the serving release", &error))?
                .ok_or_else(|| {
                    contract(
                        "read the serving release",
                        format!("release {} is not installed", admission.manifest_digest),
                    )
                })?
                .get(0);
            let (manifest, _) = ServingManifest::from_canonical_bytes(&bytes)
                .map_err(|error| contract("read the serving release", error.to_string()))?;
            let served = gate_workflow_document(tenant, admission.application, &manifest, &graph)
                .is_ok_and(|hash| hash.as_str() == wiring_hash);
            let components =
                match resolve_workflow_components(tenant, admission.application, &manifest, &graph)
                {
                    Ok(components) if served => components,
                    _ => return Ok(ScheduleRunAdmitted::NotServed),
                };
            let node_digests = serde_json::Value::Object(
                components
                    .into_iter()
                    .map(|(node, component)| {
                        (node, serde_json::Value::String(component.component_digest))
                    })
                    .collect(),
            )
            .to_string();
            let durability: String = connection
                .query_opt(SELECT_DURABILITY_SQL, &[&tenant, &admission.environment])
                .await
                .map_err(|error| storage("read the environment policy", &error))?
                .ok_or_else(|| {
                    contract(
                        "read the environment policy",
                        "the environment policy is absent".to_owned(),
                    )
                })?
                .get(0);
            let principal = workflow_user_id().to_string();
            let role = execution_role.unwrap_or_else(|| ADMIN_ROLE.to_owned());
            let due = due_text(admission.due);
            let idempotency_key = format!(
                "schedule:{}/{}:{due}",
                admission.application, admission.workflow_id
            );
            let input = serde_json::json!({ "due": due }).to_string();
            let binds: [&(dyn tokio_postgres::types::ToSql + Sync); 13] = [
                &tenant,
                &admission.application,
                &admission.manifest_digest,
                &admission.environment,
                &admission.workflow_id,
                &version,
                &wiring_hash,
                &principal,
                &role,
                &node_digests,
                &idempotency_key,
                &input,
                &durability,
            ];
            let inserted = connection
                .query_opt(&insert_workflow_schedule_run_sql(), &binds)
                .await
                .map_err(|error| storage("admit the schedule run", &error))?;
            if let Some(row) = inserted {
                let run_id: String = row.get(0);
                connection
                    .execute(insert_run_queue_sql(), &[&tenant, &run_id])
                    .await
                    .map_err(|error| storage("queue the schedule run", &error))?;
                return Ok(ScheduleRunAdmitted::Queued { run_id });
            }
            Ok(
                match connection
                    .query_opt(
                        select_workflow_schedule_run_sql(),
                        &[&tenant, &idempotency_key, &input],
                    )
                    .await
                    .map_err(|error| storage("read the first schedule run", &error))?
                {
                    Some(row) => ScheduleRunAdmitted::Queued { run_id: row.get(0) },
                    None => ScheduleRunAdmitted::Conflict,
                },
            )
        }
        .await;
        finish_queue_transaction(self, connection, result, "commit the schedule admission").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The due spelling is the schedule task's
    /// (`wamn_workflow::schedule::due_text`).
    #[test]
    fn due_text_is_whole_seconds_in_utc() {
        let due = DateTime::parse_from_rfc3339("2026-10-20T10:15:00-04:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(due_text(due), "2026-10-20T14:15:00Z");
    }
}

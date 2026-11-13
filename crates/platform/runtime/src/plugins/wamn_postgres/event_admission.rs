//! Host admission of manual and schedule workflow runs.
//!
//! A manual workflow run (docs/architecture/execution.md#workflows) enters the
//! queue the same way: the host admits it on `workflow.start` against the
//! release it serves, with the same credential, in one transaction.
//!
//! A schedule workflow run (docs/architecture/execution.md#workflows) is
//! admitted by the host at a due moment, with the same credential, in one
//! transaction that also reads the enabled version and conditions it against the
//! release.

use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::Row as _;
use wamn_catalog::{ServingManifest, condition_workflow_document, resolve_workflow_components};
use wamn_identity_protocol::{ADMIN_ROLE, PlatformComponent, workflow_user_id};
use wamn_run_state::queue::{
    insert_manual_run_sql, insert_run_queue_sql, insert_workflow_schedule_run_sql,
    select_manual_run_sql, select_workflow_schedule_run_sql,
};

use super::WamnPostgres;
use super::production_lease::{
    ProductionLeaseError, ProductionLeaseErrorType, finish_queue_transaction,
    require_executor_authority, storage,
};
use super::workflow_resolution::{PINNED_RELEASE_SQL, parse_graph};
use crate::plugins::wamn_postgres::AuthorityClass;

/// One manual workflow run to admit: the enabled workflow version, the
/// release it conditioned against, who it executes as, who started it, and the
/// component digest of every node.
#[derive(Debug, Clone, Copy)]
pub struct ManualRunAdmission<'a> {
    /// The application, by the name the environment declares: the run's
    /// package id.
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
    /// Admit one manual workflow run in `project` for `project_env_key`: one `runs`
    /// row of the manual grain and one `run_queue` row, in one transaction
    /// under the executor credential.
    pub async fn admit_manual_run(
        &self,
        project: &str,
        project_env_key: &str,
        admission: &ManualRunAdmission<'_>,
    ) -> Result<ManualRunAdmitted, ProductionLeaseError> {
        let (mut connection, policy) = self
            .checkout_platform(project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| {
                ProductionLeaseError::new(
                    ProductionLeaseErrorType::Storage,
                    "checkout manual admission connection",
                    format!("{error:?}"),
                )
            })?;
        let executor = PlatformComponent::Executor.principal_id().to_string();
        if let Err(error) = self
            .begin_with_identity(
                &mut connection,
                AuthorityClass::ExecutorPlatform,
                false,
                project_env_key,
                Some("wamn_run"),
                None,
                None,
                Some(&executor),
                Some(&PlatformComponent::Executor.principal_name()),
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(ProductionLeaseError::new(
                ProductionLeaseErrorType::Storage,
                "begin manual admission transaction",
                format!("{error:?}"),
            ));
        }
        let result = async {
            require_executor_authority(&mut connection).await?;
            let input = admission.input.to_string();
            let node_digests = admission.node_digests.to_string();

            let inserted = sqlx::query(sqlx::AssertSqlSafe(insert_manual_run_sql()))
                .bind(project_env_key)
                .bind(admission.package_id)
                .bind(admission.manifest_digest)
                .bind(admission.environment)
                .bind(admission.workflow_id)
                .bind(admission.workflow_version)
                .bind(admission.wiring_hash)
                .bind(admission.execution_principal_id)
                .bind(admission.execution_role)
                .bind(admission.invoked_by_user_id)
                .bind(&node_digests)
                .bind(admission.idempotency_key)
                .bind(&input)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| storage("admit the manual run", &error))?;
            if let Some(row) = inserted {
                let run_id: String = row.get(0);
                sqlx::query(sqlx::AssertSqlSafe((insert_run_queue_sql()).to_string()))
                    .bind(project_env_key)
                    .bind(&run_id)
                    .execute(&mut *connection)
                    .await
                    .map(|result| result.rows_affected())
                    .map_err(|error| storage("queue the manual run", &error))?;
                return Ok(ManualRunAdmitted::Queued { run_id });
            }
            Ok(
                match sqlx::query(sqlx::AssertSqlSafe((select_manual_run_sql()).to_string()))
                    .bind(project_env_key)
                    .bind(admission.idempotency_key)
                    .bind(&input)
                    .fetch_optional(&mut *connection)
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

/// The enabled version of one schedule workflow. Binds: `$1` `project_env_key`, `$2`
/// environment, `$3` application, `$4` workflow id.
const ENABLED_SCHEDULE_SQL: &str = "\
SELECT version, graph_json::text, wiring_hash, execution_role \
  FROM catalog.workflows \
 WHERE project_env_key = $1 AND environment = $2 AND application = $3 \
   AND workflow_id = $4 AND enabled AND trigger = 'schedule'";

/// One due moment of a schedule workflow to admit, against the release the
/// host serves.
#[derive(Debug, Clone, Copy)]
pub struct ScheduleRunAdmission<'a> {
    pub environment: &'a str,
    /// The application, by the name the environment declares.
    pub application: &'a str,
    /// The packages the environment declares for `application`.
    pub application_packages: &'a std::collections::BTreeSet<String>,
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
    /// The enabled version does not meet the conditions of the release, or its graph
    /// does not hash to its stored hash. No row is written.
    NotServed,
    /// The key belongs to a run of another grain or input.
    Conflict,
}

/// The canonical UTC spelling of a due moment, in schedule keys and input
/// and in the schedule task's logs.
pub fn due_text(due: DateTime<Utc>) -> String {
    due.to_rfc3339_opts(SecondsFormat::Secs, true)
}

impl WamnPostgres {
    /// Admit the run of schedule workflow `workflow_id` due at `due`, in
    /// `project` for `project_env_key`: in one transaction under the executor
    /// credential, read the enabled schedule version, condition its graph against
    /// the release `manifest_digest`, freeze the component digest of every
    /// node, and write one `runs` row of the schedule workflow grain and one
    /// `run_queue` row. The run executes as the `workflow` user with the
    /// execution role, or with `admin` for `invoker`. The key
    /// `schedule:{application}/{workflow_id}:{due}` makes a second admission
    /// of the same due moment answer the first run.
    pub async fn admit_schedule_run(
        &self,
        project: &str,
        project_env_key: &str,
        admission: &ScheduleRunAdmission<'_>,
    ) -> Result<ScheduleRunAdmitted, ProductionLeaseError> {
        let (mut connection, policy) = self
            .checkout_platform(project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| {
                ProductionLeaseError::new(
                    ProductionLeaseErrorType::Storage,
                    "checkout schedule admission connection",
                    format!("{error:?}"),
                )
            })?;
        let executor = PlatformComponent::Executor.principal_id().to_string();
        if let Err(error) = self
            .begin_with_identity(
                &mut connection,
                AuthorityClass::ExecutorPlatform,
                false,
                project_env_key,
                Some("wamn_run"),
                None,
                None,
                Some(&executor),
                Some(&PlatformComponent::Executor.principal_name()),
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(ProductionLeaseError::new(
                ProductionLeaseErrorType::Storage,
                "begin schedule admission transaction",
                format!("{error:?}"),
            ));
        }
        let result = async {
            require_executor_authority(&mut connection).await?;
            let Some(workflow) = sqlx::query(ENABLED_SCHEDULE_SQL)
                .bind(project_env_key)
                .bind(admission.environment)
                .bind(admission.application)
                .bind(admission.workflow_id)
                .fetch_optional(&mut *connection)
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
                ProductionLeaseError::new(ProductionLeaseErrorType::Contract, operation, detail)
            };
            let graph = parse_graph(&graph_json).map_err(|error| {
                contract("parse the stored workflow graph", format!("{error:#}"))
            })?;
            let bytes: Vec<u8> = sqlx::query(PINNED_RELEASE_SQL)
                .bind(project_env_key)
                .bind(admission.manifest_digest)
                .fetch_optional(&mut *connection)
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
            let packages = admission.application_packages;
            let served = condition_workflow_document(project_env_key, packages, &manifest, &graph)
                .is_ok_and(|hash| hash.as_str() == wiring_hash);
            let components =
                match resolve_workflow_components(project_env_key, packages, &manifest, &graph) {
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
            let principal = workflow_user_id().to_string();
            let role = execution_role.unwrap_or_else(|| ADMIN_ROLE.to_owned());
            let due = due_text(admission.due);
            let idempotency_key = format!(
                "schedule:{}/{}:{due}",
                admission.application, admission.workflow_id
            );
            let input = serde_json::json!({ "due": due }).to_string();

            let inserted = sqlx::query(sqlx::AssertSqlSafe(insert_workflow_schedule_run_sql()))
                .bind(project_env_key)
                .bind(admission.application)
                .bind(admission.manifest_digest)
                .bind(admission.environment)
                .bind(admission.workflow_id)
                .bind(version)
                .bind(&wiring_hash)
                .bind(&principal)
                .bind(&role)
                .bind(&node_digests)
                .bind(&idempotency_key)
                .bind(&input)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| storage("admit the schedule run", &error))?;
            if let Some(row) = inserted {
                let run_id: String = row.get(0);
                sqlx::query(sqlx::AssertSqlSafe((insert_run_queue_sql()).to_string()))
                    .bind(project_env_key)
                    .bind(&run_id)
                    .execute(&mut *connection)
                    .await
                    .map(|result| result.rows_affected())
                    .map_err(|error| storage("queue the schedule run", &error))?;
                return Ok(ScheduleRunAdmitted::Queued { run_id });
            }
            Ok(
                match sqlx::query(sqlx::AssertSqlSafe(
                    (select_workflow_schedule_run_sql()).to_string(),
                ))
                .bind(project_env_key)
                .bind(&idempotency_key)
                .bind(&input)
                .fetch_optional(&mut *connection)
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

    /// A due moment is spelled in whole seconds in UTC.
    #[test]
    fn due_text_is_whole_seconds_in_utc() {
        let due = DateTime::parse_from_rfc3339("2026-10-20T10:15:00-04:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(due_text(due), "2026-10-20T14:15:00Z");
    }
}

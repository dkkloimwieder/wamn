//! Host admission of an event-started workflow run.
//!
//! A workflow registration's event enters the queue under the authority of
//! the release that declares the workflow, not a caller's (owner ruling on
//! `wamn-upl3.6`). The host already holds that release, so it admits the run
//! with the executor credential: one `runs` row of the event grain and one
//! `run_queue` row, in one transaction. The executor's column-exact INSERT
//! reaches no service principal, binding world, or legacy flow column, so the
//! run-plane grain check admits only an event run from it.

use wamn_run_state::queue::{insert_event_run_sql, insert_run_queue_sql, select_event_run_sql};

use super::WamnPostgres;
use super::production_claim::{
    ProductionClaimError, ProductionClaimErrorKind, finish_queue_transaction,
    require_executor_authority, storage,
};
use crate::plugins::wamn_postgres::AuthorityClass;

/// One event run to admit: the released wiring, the registration whose event
/// started it, and the delivery that carried the event.
#[derive(Debug, Clone, Copy)]
pub struct EventRunAdmission<'a> {
    pub package_id: &'a str,
    pub effective_release_id: i32,
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
                ProductionClaimErrorKind::Identity,
                "resolve event admission tenant",
                "component has no host-injected tenant",
            )
        })?;
        let project = self.project_for(component_id);
        let schema = self.schema_for(component_id);
        let runner = self.runner_for(component_id);
        let user_id = self.user_id_for(component_id);
        let operation = self.operation_for(component_id);
        let (connection, policy) = self
            .checkout_platform(&project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| {
                ProductionClaimError::new(
                    ProductionClaimErrorKind::Storage,
                    "checkout event admission connection",
                    format!("{error:?}"),
                )
            })?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                &tenant,
                schema.as_deref(),
                runner.as_deref(),
                None,
                user_id.as_deref(),
                operation.as_deref(),
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
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
                        ProductionClaimErrorKind::Contract,
                        "read the environment policy",
                        "the environment policy is absent",
                    )
                })?
                .get(0);
            let input = admission.input.to_string();
            let binds: [&(dyn tokio_postgres::types::ToSql + Sync); 10] = [
                &tenant,
                &admission.package_id,
                &admission.effective_release_id,
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

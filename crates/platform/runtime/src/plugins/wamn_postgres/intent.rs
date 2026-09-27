//! [`PostgresIntentStore`]: the route intent record in the cloud (`wamn-an24`).
//!
//! The store keeps the edge's intent rules (`docs/plan/edge.md` 4.7) over
//! `wamn_run.intents`, with the statements of `wamn_run_state::intent_sql`.
//! Each call checks out an executor-platform connection, binds the tenant
//! claims of its component, runs one statement and commits, so no transaction
//! spans the export call that the intent guards.

use std::sync::Arc;

use async_trait::async_trait;
use deadpool_postgres::Object;
use wamn_run_state::IntentStore;
use wamn_run_state::intent_sql::{
    begin_intent_sql, finish_intent_sql, resolve_intent_sql, uncertain_intents_sql,
};
use wamn_run_state::intent_store::{
    Begun, Intent, IntentId, StoreError, StoreErrorKind, StoredOutcome, UncertainIntent,
};
use wamn_run_state::operator_action::OperatorActionBasis;

use super::WamnPostgres;
use crate::plugins::wamn_postgres::AuthorityClass;

/// The intent record of the routes of one tenant, bound through the session
/// claims of `component_id`.
#[derive(Clone)]
pub struct PostgresIntentStore {
    postgres: Arc<WamnPostgres>,
    component_id: String,
}

impl std::fmt::Debug for PostgresIntentStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PostgresIntentStore")
            .field("component_id", &self.component_id)
            .finish_non_exhaustive()
    }
}

impl PostgresIntentStore {
    /// A store for the tenant, project and schema bound to `component_id`.
    pub fn new(postgres: Arc<WamnPostgres>, component_id: impl Into<String>) -> Self {
        Self {
            postgres,
            component_id: component_id.into(),
        }
    }

    fn tenant(&self, operation: &'static str) -> Result<String, StoreError> {
        self.postgres
            .tenant_for(&self.component_id)
            .ok_or_else(|| contract(operation, "the store's component has no tenant".to_owned()))
    }

    /// Run `call` in one claims transaction on an executor-platform
    /// connection, and commit when it succeeds.
    async fn transact<T>(
        &self,
        operation: &'static str,
        call: impl AsyncFnOnce(&Object) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let id = &self.component_id;
        let tenant = self.tenant(operation)?;
        let project = self.postgres.project_for(id);
        let (connection, policy) = self
            .postgres
            .checkout_platform(&project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| storage(operation, format!("checkout: {error:?}")))?;
        if let Err(error) = self
            .postgres
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                &tenant,
                self.postgres.schema_for(id).as_deref(),
                self.postgres.runner_for(id).as_deref(),
                None,
                self.postgres.user_id_for(id).as_deref(),
                self.postgres.operation_for(id).as_deref(),
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.postgres.destroy(connection);
            return Err(storage(operation, format!("begin: {error:?}")));
        }
        let result = call(&connection).await;
        let end = if result.is_ok() { "COMMIT" } else { "ROLLBACK" };
        if let Err(error) = connection.batch_execute(end).await {
            self.postgres.destroy(connection);
            return Err(storage(operation, format!("{end}: {error}")));
        }
        result
    }
}

#[async_trait]
impl IntentStore for PostgresIntentStore {
    async fn begin(&self, intent: &Intent<'_>) -> Result<Begun, StoreError> {
        if intent.tenant != self.tenant("begin")? {
            return Err(contract(
                "begin",
                format!("intent tenant {} is not the store's tenant", intent.tenant),
            ));
        }
        let deadline_ms = i64::try_from(intent.deadline_ms).map_err(|_| {
            contract(
                "begin",
                format!("deadline {} ms does not fit", intent.deadline_ms),
            )
        })?;
        let sql = begin_intent_sql();
        let row = self
            .transact("begin", async |connection| {
                connection
                    .query_opt(
                        &sql,
                        &[
                            &intent.tenant,
                            &intent.release,
                            &intent.package,
                            &intent.operation,
                            &intent.idempotency_key,
                            &intent.input_hash,
                            &deadline_ms,
                        ],
                    )
                    .await
                    .map_err(|error| storage("begin", error.to_string()))
            })
            .await?
            // The same key began in another call at this instant; its row is
            // not yet visible, so this call fails and the caller retries.
            .ok_or_else(|| storage("begin", "the key began concurrently".to_owned()))?;
        let id = IntentId(row.get::<_, i64>(0).to_string());
        let input_hash: Option<String> = row.get(2);
        let outcome_kind: Option<String> = row.get(3);
        let outcome: Option<String> = row.get(4);
        let basis: Option<String> = row.get(5);
        Ok(
            match (
                row.get::<_, bool>(1),
                input_hash,
                outcome_kind,
                outcome,
                basis,
            ) {
                (true, ..) => Begun::New(id),
                (false, Some(hash), ..) if hash != intent.input_hash => Begun::Conflict(id),
                (false, _, Some(kind), Some(outcome), _) => {
                    Begun::Finished(stored_outcome(&kind, &outcome)?)
                }
                (false, _, _, _, Some(basis)) => Begun::Resolved {
                    id,
                    basis: basis
                        .parse()
                        .map_err(|error| contract("begin", format!("stored basis: {error}")))?,
                },
                (false, ..) => Begun::Uncertain(id),
            },
        )
    }

    async fn finish(&self, id: &IntentId, outcome: &StoredOutcome) -> Result<(), StoreError> {
        let row = row_id("finish", id)?;
        let (kind, value) = match outcome {
            StoredOutcome::Completed(value) => ("completed", value),
            StoredOutcome::Failed(value) => ("failed", value),
        };
        let value = serde_json::to_string(value)
            .map_err(|error| contract("finish", format!("encode outcome: {error}")))?;
        let sql = finish_intent_sql();
        let changed = self
            .transact("finish", async |connection| {
                connection
                    .execute(&sql, &[&row, &kind, &value])
                    .await
                    .map_err(|error| storage("finish", error.to_string()))
            })
            .await?;
        if changed == 0 {
            return Err(contract("finish", format!("intent {row} is not open")));
        }
        Ok(())
    }

    async fn uncertain(&self, limit: u32) -> Result<Vec<UncertainIntent>, StoreError> {
        let limit = i64::from(limit);
        let rows = self
            .transact("uncertain", async |connection| {
                connection
                    .query(uncertain_intents_sql(), &[&limit])
                    .await
                    .map_err(|error| storage("uncertain", error.to_string()))
            })
            .await?;
        Ok(rows
            .iter()
            .map(|row| UncertainIntent {
                id: IntentId(row.get::<_, i64>(0).to_string()),
                tenant: row.get(1),
                release: row.get(2),
                package: row.get(3),
                operation: row.get(4),
                idempotency_key: row.get(5),
            })
            .collect())
    }

    async fn resolve(&self, id: &IntentId, basis: OperatorActionBasis) -> Result<(), StoreError> {
        let row = row_id("resolve", id)?;
        let sql = resolve_intent_sql();
        let changed = self
            .transact("resolve", async |connection| {
                connection
                    .execute(&sql, &[&row, &basis.as_str()])
                    .await
                    .map_err(|error| storage("resolve", error.to_string()))
            })
            .await?;
        if changed == 0 {
            return Err(contract(
                "resolve",
                format!("intent {row} is not uncertain"),
            ));
        }
        Ok(())
    }
}

fn stored_outcome(kind: &str, outcome: &str) -> Result<StoredOutcome, StoreError> {
    let value = serde_json::from_str(outcome)
        .map_err(|error| contract("begin", format!("stored outcome is not JSON: {error}")))?;
    match kind {
        "completed" => Ok(StoredOutcome::Completed(value)),
        "failed" => Ok(StoredOutcome::Failed(value)),
        other => Err(contract("begin", format!("stored outcome kind {other}"))),
    }
}

/// The row id that an [`IntentId`] names as decimal text.
fn row_id(operation: &'static str, id: &IntentId) -> Result<i64, StoreError> {
    id.0.parse()
        .map_err(|_| contract(operation, format!("intent id {:?} is not a row id", id.0)))
}

fn storage(operation: &'static str, detail: String) -> StoreError {
    StoreError::new(StoreErrorKind::Storage, operation, detail)
}

fn contract(operation: &'static str, detail: String) -> StoreError {
    StoreError::new(StoreErrorKind::Contract, operation, detail)
}

//! The write log, `platform.write_log`, as the engine's intent store
//! (`docs/history/host-transaction.md` 4.2 and 4.3).
//!
//! An operation with SQL claims its key in its request element transaction, so the claim
//! and the work commit together or not at all: [`OperationTransaction`] is
//! that store. An operation without SQL claims in two steps on a connection of
//! its own: [`WriteLogStore`] commits the claim before the call and the result
//! after it, and a claim that never finishes keeps a null `result`, which is
//! uncertain.
//!
//! The row's `request` holds the engine's hash or the entry's unchanged hash
//! bytes. Its `operation` is the operation without its `@version`, so a retry
//! across a release answers the stored result. The entry boundary returns
//! result text without parsing it.

use sqlx::Row as _;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::postgres::PgRow as Row;
use wamn_run_state::IntentStore;
use wamn_run_state::intent_store::{
    Begun, Intent, IntentId, StoreError, StoreErrorType, StoredOutcome, UncertainIntent,
};
use wamn_run_state::operator_action::OperatorActionBasis;

use super::operation_transaction::OperationTransaction;
use super::resources::{StatementConnectionGuard, take_conn};
use super::types::PgParam;
use super::{PgError, SqlValue, WamnPostgres};

/// The complete success body of a generated delete, before request correlation.
pub const DELETED_RESULT: &str = r#"{"value":{"outcome":"deleted"}}"#;

/// Insert the claim of one key, or nothing when a committed row holds the key.
///
/// An uncommitted row of another transaction makes this insert wait until that
/// transaction ends. A commit makes it return no row, and a rollback lets it
/// insert.
pub const CLAIM_SQL: &str = "INSERT INTO platform.write_log (operation, idempotency_key, request) \
     VALUES ($1::text, $2::text, $3::bytea) \
     ON CONFLICT (operation, idempotency_key) DO NOTHING \
     RETURNING idempotency_key";

/// Insert a known success result with a claim in the transaction of its work.
const CLAIM_RESULT_SQL: &str = "INSERT INTO platform.write_log (operation, idempotency_key, request, result) \
     VALUES ($1::text, $2::text, $3::bytea, $4::text) \
     ON CONFLICT (operation, idempotency_key) DO NOTHING \
     RETURNING idempotency_key";

/// Bind a known result only when the claim and the work share a transaction.
fn claim_statement(
    mut params: Vec<SqlValue>,
    result: Option<&str>,
) -> (&'static str, Vec<SqlValue>) {
    if let Some(result) = result {
        params.push(SqlValue::Text(result.to_owned()));
        (CLAIM_RESULT_SQL, params)
    } else {
        (CLAIM_SQL, params)
    }
}

/// Read the request and the result that a committed claim of one key holds.
pub const READ_SQL: &str = "SELECT request, result FROM platform.write_log \
     WHERE operation = $1::text AND idempotency_key = $2::text";

/// Store the result in a claim that has none.
pub const FINISH_SQL: &str = "UPDATE platform.write_log SET result = $3::text \
     WHERE operation = $1::text AND idempotency_key = $2::text AND result IS NULL \
     RETURNING idempotency_key";

/// Abort on a missing claim because COMMIT is already queued behind this statement.
pub(super) const COMMIT_FINISH_SQL: &str = "WITH finished AS (UPDATE platform.write_log SET result = $3::text \
     WHERE operation = $1::text AND idempotency_key = $2::text AND result IS NULL \
     RETURNING idempotency_key) SELECT 1 / count(*)::integer FROM finished";

/// Free the key of a two-step claim whose request element was refused.
const RELEASE_SQL: &str = "DELETE FROM platform.write_log \
     WHERE operation = $1::text AND idempotency_key = $2::text AND result IS NULL \
     RETURNING idempotency_key";

/// The two-step claims that never finished, oldest first.
const UNCERTAIN_SQL: &str = "SELECT operation, idempotency_key FROM platform.write_log \
     WHERE result IS NULL ORDER BY created_at, operation, idempotency_key LIMIT $1::bigint";

/// The write log operation of one operation: the operation without its
/// `@version`.
fn log_operation(operation: &str) -> &str {
    operation
        .split_once('@')
        .map_or(operation, |(operation, _)| operation)
}

/// The intent id of one claim: its log operation and its key.
fn intent_id(operation: &str, key: &str) -> IntentId {
    IntentId(format!("{operation}#{key}"))
}

/// The log operation and the key that an intent id names. An operation id
/// has no `#`, so the first one ends it.
fn claim_of<'a>(
    operation: &'static str,
    id: &'a IntentId,
) -> Result<(&'a str, &'a str), StoreError> {
    id.0.split_once('#').ok_or_else(|| {
        StoreError::new(
            StoreErrorType::Contract,
            operation,
            format!("intent id {:?} is not a write log claim", id.0),
        )
    })
}

fn storage(operation: &'static str) -> impl Fn(PgError) -> StoreError {
    move |error| StoreError::new(StoreErrorType::Storage, operation, format!("{error:?}"))
}

fn contract(operation: &'static str, detail: String) -> StoreError {
    StoreError::new(StoreErrorType::Contract, operation, detail)
}

/// Run one write log statement on `run`, and answer the begin of `intent`.
///
/// `run` executes one statement and returns its rows.
async fn begin_with<F, Fut>(
    intent: &Intent<'_>,
    result: Option<&str>,
    run: F,
) -> Result<Begun, StoreError>
where
    F: Fn(&'static str, Vec<SqlValue>) -> Fut,
    Fut: Future<Output = Result<Vec<Row>, PgError>>,
{
    let operation = log_operation(intent.operation).to_owned();
    let key = intent.idempotency_key.to_owned();
    let request = intent.input_hash.as_bytes().to_vec();
    let id = intent_id(&operation, &key);
    let (sql, params) = claim_statement(
        vec![
            SqlValue::Text(operation.clone()),
            SqlValue::Text(key.clone()),
            SqlValue::Bytes(request.clone()),
        ],
        result,
    );
    let claimed = run(sql, params).await.map_err(storage("begin"))?;
    if !claimed.is_empty() {
        return Ok(Begun::New(id));
    }
    let rows = run(
        READ_SQL,
        vec![SqlValue::Text(operation), SqlValue::Text(key)],
    )
    .await
    .map_err(storage("begin"))?;
    let row = rows
        .first()
        .ok_or_else(|| contract("begin", format!("claim {} has no row", id.0)))?;
    let stored: Vec<u8> = row.get(0);
    let result: Option<String> = row.get(1);
    if stored != request {
        return Ok(Begun::Conflict(id));
    }
    match result {
        Some(result) => serde_json::from_str(&result)
            .map(|value| Begun::Finished(StoredOutcome(value)))
            .map_err(|error| contract("begin", format!("stored result is not JSON: {error}"))),
        None => Ok(Begun::Uncertain(id)),
    }
}

/// Store `outcome` in the claim `id` through `run`.
async fn finish_with<F, Fut>(
    id: &IntentId,
    outcome: &StoredOutcome,
    run: F,
) -> Result<(), StoreError>
where
    F: Fn(&'static str, Vec<SqlValue>) -> Fut,
    Fut: Future<Output = Result<Vec<Row>, PgError>>,
{
    let (operation, key) = claim_of("finish", id)?;
    let result = serde_json::to_string(&outcome.0)
        .map_err(|error| contract("finish", format!("encode outcome: {error}")))?;
    let rows = run(
        FINISH_SQL,
        vec![
            SqlValue::Text(operation.to_owned()),
            SqlValue::Text(key.to_owned()),
            SqlValue::Text(result),
        ],
    )
    .await
    .map_err(storage("finish"))?;
    if rows.is_empty() {
        return Err(contract("finish", format!("claim {} is not open", id.0)));
    }
    Ok(())
}

impl OperationTransaction {
    /// Claim application-owned bytes without interpreting the hash or result.
    pub(super) async fn begin_opaque(
        &self,
        operation: &str,
        key: &str,
        request: Vec<u8>,
    ) -> Result<OpaqueBegun, PgError> {
        let operation = log_operation(operation).to_owned();
        let (sql, params) = claim_statement(
            vec![
                SqlValue::Text(operation.clone()),
                SqlValue::Text(key.to_owned()),
                SqlValue::Bytes(request.clone()),
            ],
            self.known_result.as_deref(),
        );
        let claimed = self.log_statement(sql, params).await?;
        if !claimed.is_empty() {
            return Ok(OpaqueBegun::New);
        }
        let rows = self
            .log_statement(
                READ_SQL,
                vec![SqlValue::Text(operation), SqlValue::Text(key.to_owned())],
            )
            .await?;
        let row = rows.first().ok_or_else(|| {
            PgError::QueryError((
                "P0002".to_owned(),
                "the write log claim has no row".to_owned(),
            ))
        })?;
        let stored: Vec<u8> = row.get(0);
        let result: Option<String> = row.get(1);
        if stored != request {
            return Ok(OpaqueBegun::Conflict);
        }
        Ok(match result {
            Some(result) => OpaqueBegun::Finished(result),
            None => OpaqueBegun::Uncertain,
        })
    }

    /// Stage the application's result text for the existing commit pipeline.
    pub(super) fn finish_opaque(&self, operation: &str, key: &str, result: String) {
        if self.known_result.is_some() {
            return;
        }
        *self
            .finish
            .lock()
            .expect("request element finish lock poisoned") =
            Some([log_operation(operation).to_owned(), key.to_owned(), result]);
    }

    /// Run one write log statement on this transaction's connection.
    async fn log_statement(
        &self,
        sql: &'static str,
        params: Vec<SqlValue>,
    ) -> Result<Vec<Row>, PgError> {
        let state = &self.transaction().state;
        let mut connection = StatementConnectionGuard::new(
            take_conn(state)?,
            Arc::clone(&self.transaction().destroyed),
        );
        let result = async {
            let identity = self
                .identity
                .lock()
                .expect("request element identity lock poisoned")
                .take();
            if let Some(identity) = identity {
                identity.bind(connection.connection()).await?;
            }
            let mut query = sqlx::query(sql);
            for value in &params {
                query = query.bind(PgParam(value, None));
            }
            query
                .fetch_all(connection.connection())
                .await
                .map_err(|error| super::types::map_pg_error(&error))
        }
        .await;
        match result {
            Ok(rows) => {
                connection.restore(state);
                Ok(rows)
            }
            Err(error) => {
                // A failed statement aborts the transaction, so its connection
                // goes: the owner's commit then refuses, and nothing commits.
                if let Ok(mut transaction) = state.lock() {
                    transaction.finished = true;
                }
                drop(connection);
                Err(error)
            }
        }
    }
}

/// The write log has no operator resolution, so it never returns `resolved`.
#[derive(Debug)]
pub(super) enum OpaqueBegun {
    New,
    Finished(String),
    Uncertain,
    Conflict,
}

/// The claim of an operation with SQL, inside its request element transaction.
///
/// The claim commits with the work, so nothing here is ever uncertain, and a
/// refused request element frees its key when the owner rolls the transaction back.
#[async_trait]
impl IntentStore for OperationTransaction {
    async fn begin(&self, intent: &Intent<'_>) -> Result<Begun, StoreError> {
        begin_with(intent, self.known_result.as_deref(), |sql, params| {
            self.log_statement(sql, params)
        })
        .await
    }

    /// Stage the guest result for the owner's finish-and-commit pipeline.
    async fn finish(&self, id: &IntentId, outcome: &StoredOutcome) -> Result<(), StoreError> {
        if self.known_result.is_some() {
            return Ok(());
        }
        let (operation, key) = claim_of("finish", id)?;
        let result = serde_json::to_string(&outcome.0)
            .map_err(|error| contract("finish", format!("encode outcome: {error}")))?;
        let mut finish = self
            .finish
            .lock()
            .expect("request element finish lock poisoned");
        if finish.is_some() {
            return Err(contract(
                "finish",
                "the request element already has a pending finish".to_owned(),
            ));
        }
        *finish = Some([operation.to_owned(), key.to_owned(), result]);
        Ok(())
    }

    /// The owner's rollback undoes the claim.
    async fn release(&self, _id: &IntentId) -> Result<(), StoreError> {
        Ok(())
    }

    /// A claim in a transaction commits with its result, so none is open.
    async fn uncertain(&self, _limit: u32) -> Result<Vec<UncertainIntent>, StoreError> {
        Ok(Vec::new())
    }

    async fn resolve(&self, id: &IntentId, _basis: OperatorActionBasis) -> Result<(), StoreError> {
        Err(contract(
            "resolve",
            format!("claim {} is not uncertain", id.0),
        ))
    }
}

/// The two-step claims of operations without SQL, in the write log of one
/// project database, each statement on a connection of its own.
#[derive(Debug, Clone)]
pub struct WriteLogStore {
    postgres: Arc<WamnPostgres>,
    project: String,
    project_env_key: String,
}

impl WriteLogStore {
    /// The write log of `project_env_key`'s database in `project`.
    pub fn new(postgres: Arc<WamnPostgres>, project: String, project_env_key: String) -> Self {
        Self {
            postgres,
            project,
            project_env_key,
        }
    }

    /// Run one write log statement alone, so it commits at once.
    async fn statement(
        &self,
        sql: &'static str,
        params: Vec<SqlValue>,
    ) -> Result<Vec<Row>, PgError> {
        let (mut connection, _pool) = self
            .postgres
            .checkout_guest(&self.project, &self.project_env_key)
            .await?;
        let mut query = sqlx::query(sql);
        for value in &params {
            query = query.bind(PgParam(value, None));
        }
        let result = query.fetch_all(&mut *connection).await;
        match result {
            Ok(rows) => Ok(rows),
            Err(error) => {
                self.postgres.destroy(connection);
                Err(super::types::map_pg_error(&error))
            }
        }
    }
}

#[async_trait]
impl IntentStore for WriteLogStore {
    async fn begin(&self, intent: &Intent<'_>) -> Result<Begun, StoreError> {
        if intent.partition != self.project_env_key {
            return Err(contract(
                "begin",
                format!(
                    "the write log of {} holds no key of {}",
                    self.project_env_key, intent.partition
                ),
            ));
        }
        begin_with(intent, None, |sql, params| self.statement(sql, params)).await
    }

    async fn finish(&self, id: &IntentId, outcome: &StoredOutcome) -> Result<(), StoreError> {
        finish_with(id, outcome, |sql, params| self.statement(sql, params)).await
    }

    async fn release(&self, id: &IntentId) -> Result<(), StoreError> {
        let (operation, key) = claim_of("release", id)?;
        let rows = self
            .statement(
                RELEASE_SQL,
                vec![
                    SqlValue::Text(operation.to_owned()),
                    SqlValue::Text(key.to_owned()),
                ],
            )
            .await
            .map_err(storage("release"))?;
        if rows.is_empty() {
            return Err(contract("release", format!("claim {} is not open", id.0)));
        }
        Ok(())
    }

    async fn uncertain(&self, limit: u32) -> Result<Vec<UncertainIntent>, StoreError> {
        let rows = self
            .statement(UNCERTAIN_SQL, vec![SqlValue::Int64(i64::from(limit))])
            .await
            .map_err(storage("uncertain"))?;
        Ok(rows
            .iter()
            .map(|row| {
                let operation: String = row.get(0);
                let key: String = row.get(1);
                UncertainIntent {
                    id: intent_id(&operation, &key),
                    partition: self.project_env_key.clone(),
                    // The write log keeps neither: its key is the operation.
                    release: String::new(),
                    package: operation
                        .split_once(':')
                        .map_or_else(String::new, |(package, _)| package.to_owned()),
                    operation,
                    idempotency_key: key,
                }
            })
            .collect())
    }

    /// The write log has no operator resolution. The client sends a new key.
    async fn resolve(&self, id: &IntentId, _basis: OperatorActionBasis) -> Result<(), StoreError> {
        Err(contract(
            "resolve",
            format!(
                "the write log resolves no claim, and {} stays uncertain",
                id.0
            ),
        ))
    }
}

#[cfg(test)]
mod tests;

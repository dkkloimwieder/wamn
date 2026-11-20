//! Concrete operation claims and outcomes in `platform.write_log`.
//! (`docs/history/host-transaction.md` 4.2 and 4.3).
//!
//! An operation with SQL claims its key in its request element transaction, so the claim
//! and the work commit together or not at all: [`OperationTransaction`] is
//! that store. An operation without SQL claims in two steps on a connection of
//! its own: [`WriteLogStore`] commits the claim before the call and the result
//! after it, and a claim that never finishes keeps a null `result`, which is
//! unfinished until it records a result or retention removes it.
//!
//! The row's `request` holds the engine's hash or the entry's unchanged hash
//! bytes. Its `operation` is the operation without its `@version`, so a retry
//! across a release answers the stored result. The entry boundary returns
//! result text without parsing it.

use futures_util::TryStreamExt as _;
use postgres_protocol::escape;
use sqlx::Row as _;
use sqlx::postgres::PgRow as Row;
use std::fmt::Write as _;
use std::sync::Arc;

use super::operation_transaction::OperationTransaction;
use super::resources::{StatementConnectionGuard, take_conn};
use wamn_postgres::types::{PgParam, map_pg_error};
use super::{PgError, SqlValue, OperationHost};

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

/// Read the request and the result that a committed claim of one key holds.
pub const READ_SQL: &str = "SELECT request, result, extract(epoch FROM (clock_timestamp() - created_at))::float8 FROM platform.write_log \
     WHERE operation = $1::text AND idempotency_key = $2::text";

/// Store the result in a claim that has none.
pub const FINISH_SQL: &str = "UPDATE platform.write_log SET result = $3::text \
     WHERE operation = $1::text AND idempotency_key = $2::text AND result IS NULL \
     RETURNING idempotency_key";

/// Whether the identity check completed before a commit batch failed.
#[derive(Debug)]
pub(super) struct CommitError {
    pub(super) source: sqlx::Error,
    pub(super) identity_checked: bool,
}

/// Check identity, finish the claim, emit causation and commit in one batch.
pub(super) async fn commit_transaction(
    connection: &mut sqlx::PgConnection,
    xid: &str,
    finish: Option<&[String; 3]>,
    run: Option<&wamn_event::Causation>,
) -> Result<(), CommitError> {
    let mut batch = format!(
        "SELECT 1 / COALESCE(\
         ((SELECT (wamn_authority.current_identity()).xid) = {}::text::xid8)::integer, 0);",
        escape::escape_literal(xid),
    );
    if let Some([operation, key, output]) = finish {
        write!(
            batch,
            "WITH finished AS (UPDATE platform.write_log SET result = {}::text \
             WHERE operation = {}::text AND idempotency_key = {}::text AND result IS NULL \
             RETURNING idempotency_key) SELECT 1 / count(*)::integer FROM finished;",
            escape::escape_literal(output),
            escape::escape_literal(operation),
            escape::escape_literal(key),
        )
        .expect("writing write log completion to String cannot fail");
    }
    if let Some(run) = run {
        batch.push_str(&super::transactions::causation_sql(run));
    }
    batch.push_str("COMMIT;");
    let mut results = sqlx::raw_sql(sqlx::AssertSqlSafe(batch)).execute_many(connection);
    let mut identity_checked = false;
    loop {
        match results.try_next().await {
            Ok(Some(_)) => identity_checked = true,
            Ok(None) => return Ok(()),
            Err(source) => {
                return Err(CommitError {
                    source,
                    identity_checked,
                });
            }
        }
    }
}

/// Free the key of a two-step claim whose request element was refused.
const RELEASE_SQL: &str = "DELETE FROM platform.write_log \
     WHERE operation = $1::text AND idempotency_key = $2::text AND result IS NULL \
     RETURNING idempotency_key";

/// The write log operation of one operation: the operation without its
/// `@version`.
fn log_operation(operation: &wamn_catalog::canonical_name::CanonicalName) -> &str {
    operation
        .as_str()
        .split_once('@')
        .map_or(operation.as_str(), |(operation, _)| operation)
}

/// Claim opaque request bytes on either the operation transaction or an autocommit connection.
async fn claim_with<F, Fut>(
    operation: &wamn_catalog::canonical_name::CanonicalName,
    key: &str,
    request: Vec<u8>,
    run: F,
) -> Result<Option<Claim>, PgError>
where
    F: Fn(&'static str, Vec<SqlValue>) -> Fut,
    Fut: Future<Output = Result<Vec<Row>, PgError>>,
{
    let operation = log_operation(operation);
    let params = vec![
        SqlValue::Text(operation.to_owned()),
        SqlValue::Text(key.to_owned()),
        SqlValue::Bytes(request.clone()),
    ];
    if !run(CLAIM_SQL, params).await?.is_empty() {
        return Ok(Some(Claim::New));
    }
    let rows = run(
        READ_SQL,
        vec![
            SqlValue::Text(operation.to_owned()),
            SqlValue::Text(key.to_owned()),
        ],
    )
    .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let stored: Vec<u8> = row.get(0);
    let result: Option<String> = row.get(1);
    if stored != request {
        return Ok(Some(Claim::Conflict));
    }
    Ok(Some(match result {
        Some(result) => Claim::Finished(result),
        None => unfinished(row.get(2)),
    }))
}

fn opaque_claim(claim: Option<Claim>) -> Result<Claim, PgError> {
    claim.ok_or_else(|| {
        PgError::QueryError((
            "P0002".to_owned(),
            "the write log claim has no row".to_owned(),
        ))
    })
}

/// Allow five seconds for database clock adjustments relative to the host's monotonic clock.
const CLAIM_CLOCK_SKEW: std::time::Duration = std::time::Duration::from_secs(5);

fn unfinished(age_seconds: f64) -> Claim {
    if age_seconds > (wamn_engine::engine::MAX_HOST_CALL_DURATION + CLAIM_CLOCK_SKEW).as_secs_f64()
    {
        Claim::OutcomeUnknown
    } else {
        Claim::InProgress
    }
}

/// Return whether completing the claim changed an open record.
async fn complete_with<F, Fut>(
    operation: &wamn_catalog::canonical_name::CanonicalName,
    key: &str,
    result: String,
    run: F,
) -> Result<bool, PgError>
where
    F: Fn(&'static str, Vec<SqlValue>) -> Fut,
    Fut: Future<Output = Result<Vec<Row>, PgError>>,
{
    let rows = run(
        FINISH_SQL,
        vec![
            SqlValue::Text(log_operation(operation).to_owned()),
            SqlValue::Text(key.to_owned()),
            SqlValue::Text(result),
        ],
    )
    .await?;
    Ok(!rows.is_empty())
}

impl OperationTransaction {
    /// Claim application-owned bytes without interpreting the hash or result.
    pub async fn begin_opaque(
        &self,
        operation: &wamn_catalog::canonical_name::CanonicalName,
        key: &str,
        request: Vec<u8>,
    ) -> Result<Claim, PgError> {
        let claim = claim_with(operation, key, request, |sql, params| {
            self.log_statement(sql, params)
        })
        .await?;
        let claim = opaque_claim(claim)?;
        if matches!(claim, Claim::New)
            && let Some(result) = &self.known_result
        {
            self.stage_finish(operation, key, result.to_owned());
        }
        Ok(claim)
    }

    /// Stage the application's result text for the existing commit pipeline.
    pub(super) fn finish_opaque(
        &self,
        operation: &wamn_catalog::canonical_name::CanonicalName,
        key: &str,
        result: String,
    ) {
        if self.known_result.is_none() {
            self.stage_finish(operation, key, result);
        }
    }

    fn stage_finish(
        &self,
        operation: &wamn_catalog::canonical_name::CanonicalName,
        key: &str,
        result: String,
    ) {
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
        self.start().await?;
        let state = &self.transaction().state;
        let mut connection = StatementConnectionGuard::new(
            take_conn(state)?,
            Arc::clone(&self.transaction().destroyed),
        );
        let result = async {
            let mut query = sqlx::query(sql);
            for value in &params {
                query = query.bind(PgParam(value, None));
            }
            query
                .fetch_all(connection.connection())
                .await
                .map_err(|error| map_pg_error(&error))
        }
        .await;
        match result {
            Ok(rows) => {
                super::resources::restore_connection(connection, state);
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
pub enum Claim {
    New,
    Finished(String),
    InProgress,
    OutcomeUnknown,
    Conflict,
}

/// The two-step claims of operations without SQL, in the write log of one
/// project database, each statement on a connection of its own.
#[derive(Debug, Clone)]
pub struct WriteLogStore {
    postgres: Arc<OperationHost>,
    project: wamn_registry::ProjectId,
    project_env_key: wamn_registry::ProjectEnvKey,
}

fn require_open_claim(changed: bool) -> Result<(), PgError> {
    if changed {
        Ok(())
    } else {
        Err(PgError::QueryError((
            "P0002".to_owned(),
            "claim is not open".to_owned(),
        )))
    }
}

impl WriteLogStore {
    /// The write log of `project_env_key`'s database in `project`.
    pub fn new(
        postgres: Arc<OperationHost>,
        project: wamn_registry::ProjectId,
        project_env_key: wamn_registry::ProjectEnvKey,
    ) -> Self {
        Self {
            postgres,
            project,
            project_env_key,
        }
    }

    /// Commit a claim before a call that has external effects.
    pub async fn claim(
        &self,
        operation: &wamn_catalog::canonical_name::CanonicalName,
        key: &str,
        request: Vec<u8>,
    ) -> Result<Claim, PgError> {
        let claim = claim_with(operation, key, request, |sql, params| {
            self.statement(sql, params)
        })
        .await?;
        opaque_claim(claim)
    }

    /// Record late completion without changing an already completed outcome.
    pub async fn complete(
        &self,
        operation: &wamn_catalog::canonical_name::CanonicalName,
        key: &str,
        result: String,
    ) -> Result<(), PgError> {
        let changed = self.complete_claim(operation, key, result).await?;
        require_open_claim(changed)
    }

    async fn complete_claim(
        &self,
        operation: &wamn_catalog::canonical_name::CanonicalName,
        key: &str,
        result: String,
    ) -> Result<bool, PgError> {
        complete_with(operation, key, result, |sql, params| {
            self.statement(sql, params)
        })
        .await
    }

    /// Release a refused external claim without recording a result.
    pub async fn release_claim(
        &self,
        operation: &wamn_catalog::canonical_name::CanonicalName,
        key: &str,
    ) -> Result<(), PgError> {
        let changed = self.release_open_claim(operation, key).await?;
        require_open_claim(changed)
    }

    async fn release_open_claim(
        &self,
        operation: &wamn_catalog::canonical_name::CanonicalName,
        key: &str,
    ) -> Result<bool, PgError> {
        let rows = self
            .statement(
                RELEASE_SQL,
                vec![
                    SqlValue::Text(log_operation(operation).to_owned()),
                    SqlValue::Text(key.to_owned()),
                ],
            )
            .await?;
        Ok(!rows.is_empty())
    }

    /// Run one write log statement alone, so it commits at once.
    async fn statement(
        &self,
        sql: &'static str,
        params: Vec<SqlValue>,
    ) -> Result<Vec<Row>, PgError> {
        let (mut connection, _pool) = self
            .postgres
            .pools.checkout_guest(&self.project, &self.project_env_key)
            .await?;
        let mut query = sqlx::query(sql);
        for value in &params {
            query = query.bind(PgParam(value, None));
        }
        let result = query.fetch_all(&mut *connection).await;
        match result {
            Ok(rows) => Ok(rows),
            Err(error) => {
                self.postgres.pools.destroy(connection);
                Err(map_pg_error(&error))
            }
        }
    }
}

#[cfg(test)]
mod tests;

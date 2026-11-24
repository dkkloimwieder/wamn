//! Connection ownership and completion for an operation transaction.

use std::sync::{Arc, Mutex, atomic::AtomicU64};

use sqlx::Statement as _;
use wamn_application::database::{Claim, CommitFailure, SqlError, SqlRows, SqlValue};
use wamn_event::Causation;
use wamn_package::canonical_name::CanonicalName;
use wamn_registry::ApplicationName;

use wamn_postgres::{Object, destroy_connection};

use super::query::{QueryTransaction, open_stream};
use wamn_postgres::ConnectionGuard;
use wamn_postgres::{connection_lost, map_pg_error};

#[derive(Debug)]
pub(super) struct TxnState {
    conn: Option<Object>,
    pub(super) finished: bool,
}

pub(super) type SharedTxnState = Arc<Mutex<TxnState>>;

#[derive(Debug)]
pub(crate) struct Transaction {
    pub(super) state: SharedTxnState,
    pub(super) destroyed: Arc<AtomicU64>,
    pub(super) finish: Mutex<Option<[String; 3]>>,
    xid: String,
    causation: Option<Causation>,
}

impl Transaction {
    pub(super) fn new(
        connection: ConnectionGuard,
        destroyed: Arc<AtomicU64>,
        xid: String,
        causation: Option<Causation>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(TxnState {
                conn: Some(connection.into_connection()),
                finished: false,
            })),
            destroyed,
            finish: Mutex::new(None),
            xid,
            causation,
        }
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        let mut state = match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let finished = state.finished;
        state.finished = true;
        let connection = state.conn.take();
        drop(state);
        if let Some(connection) = connection {
            if finished {
                drop(connection);
            } else {
                tracing::warn!(
                    "wamn:postgres transaction dropped without commit/rollback; destroying connection"
                );
                destroy_connection(connection, &self.destroyed);
            }
        }
    }
}

impl Transaction {
    pub(crate) async fn query(
        &self,
        sql: String,
        params: Vec<SqlValue>,
    ) -> Result<SqlRows, SqlError> {
        let connection = ConnectionGuard::new(take_conn(&self.state)?, Arc::clone(&self.destroyed));
        open_stream(
            connection,
            QueryTransaction::Operation(Arc::clone(&self.state)),
            sql,
            params,
        )
        .await
    }

    pub(crate) async fn query_batch(&self, sql: String) -> Result<(), SqlError> {
        with_txn_conn(&self.state, &self.destroyed, |mut connection| async move {
            let result = sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                .execute(connection.connection())
                .await
                .map(drop);
            (connection, result)
        })
        .await
    }

    pub(crate) async fn claim(
        &self,
        operation: &CanonicalName,
        key: &str,
        request: Vec<u8>,
    ) -> Result<Claim, SqlError> {
        self.claim_request(operation, key, request).await
    }

    pub(crate) fn stage_finish(&self, operation: &CanonicalName, key: &str, result: String) {
        self.stage_result(operation, key, result);
    }

    pub(crate) async fn wal_position(&self) -> Result<String, SqlError> {
        with_txn_conn(&self.state, &self.destroyed, |mut connection| async move {
            let result = sqlx::query_scalar("SELECT pg_current_wal_lsn()::text")
                .fetch_one(connection.connection())
                .await;
            (connection, result)
        })
        .await
    }

    pub(crate) async fn counter_sequence(
        &self,
        application: &ApplicationName,
        sequence: &str,
    ) -> Result<Option<(String, String)>, SqlError> {
        with_txn_conn(&self.state, &self.destroyed, |mut connection| async move {
            let result = async {
                let raw = connection.connection();
                let statement = sqlx::Executor::prepare(
                    &mut *raw,
                    sqlx::SqlSafeStr::into_sql_str("SELECT id::text, pattern FROM platform.number_sequence WHERE application = $1 AND name = $2"),
                ).await?;
                sqlx::query_as(statement.sql().clone())
                    .bind(application.as_str())
                    .bind(sequence)
                    .fetch_optional(raw)
                    .await
            }.await;
            (connection, result)
        })
        .await
    }

    pub(crate) async fn set_counter(
        &self,
        sequence: &str,
        counter: &str,
        key: &str,
        value: i64,
    ) -> Result<(), SqlError> {
        with_txn_conn(&self.state, &self.destroyed, |mut connection| async move {
            let result = async {
                let raw = connection.connection();
                let statement = sqlx::Executor::prepare(
                    &mut *raw,
                    sqlx::SqlSafeStr::into_sql_str(
                        "SELECT platform.set_counter($1::text::uuid, $2, $3, $4)",
                    ),
                )
                .await?;
                sqlx::query(statement.sql().clone())
                    .bind(sequence)
                    .bind(counter)
                    .bind(key)
                    .bind(value)
                    .execute(raw)
                    .await
                    .map(drop)
            }
            .await;
            (connection, result)
        })
        .await
    }

    /// Commit the transaction.
    ///
    /// # Errors
    ///
    /// Returns [`CommitFailure::Uncertain`] when the guest ended the host
    /// transaction or the host did not read the answer to `COMMIT`.
    pub(crate) async fn commit(&self) -> Result<(), CommitFailure> {
        let state = &self.state;
        let connection = take_conn(state).map_err(CommitFailure::RolledBack)?;
        let mut connection = ConnectionGuard::new(connection, Arc::clone(&self.destroyed));
        let mark_finished = || {
            if let Ok(mut transaction) = state.lock() {
                transaction.finished = true;
            }
        };
        let finish = self
            .finish
            .lock()
            .expect("request element finish lock poisoned")
            .take();
        let result = super::write_log::commit_transaction(
            connection.connection(),
            &self.xid,
            finish.as_ref(),
            self.causation.as_ref(),
        )
        .await;
        match result {
            Ok(()) => {
                mark_finished();
                connection.repool();
                Ok(())
            }
            Err(failure) => {
                mark_finished();
                let error = failure.source;
                // A missing claim rolls back; a missing caller means the guest
                // may already have committed the host's work.
                let ended_early = !failure.identity_checked
                    && error
                        .as_database_error()
                        .is_some_and(|error| error.code().is_some_and(|code| code == "22012"));
                let answered = error.as_database_error().is_some();
                drop(connection);
                let error = map_pg_error(&error);
                Err(if answered && !ended_early {
                    CommitFailure::RolledBack(error)
                } else {
                    CommitFailure::Uncertain(error)
                })
            }
        }
    }

    /// Roll the transaction back.
    ///
    /// # Errors
    ///
    /// Returns the error of `ROLLBACK`. The connection is then destroyed, so
    /// the server aborts the transaction. A transaction whose connection is
    /// already lost, for example after a failed statement, is rolled back, so
    /// it answers `Ok`.
    pub(crate) async fn rollback(&self) -> Result<(), SqlError> {
        if self
            .state
            .lock()
            .is_ok_and(|state| state.finished && state.conn.is_none())
        {
            return Ok(());
        }
        match finish_statement_txn(&self.state, &self.destroyed, "ROLLBACK").await {
            Err(SqlError::ConnectionUnavailable) => Ok(()),
            finished => finished,
        }
    }
}

pub(super) fn restore_connection(connection: ConnectionGuard, state: &SharedTxnState) {
    let Ok(mut transaction) = state.lock() else {
        return;
    };
    if !transaction.finished {
        transaction.conn = Some(connection.into_connection());
    }
}

pub(super) fn take_conn(state: &SharedTxnState) -> Result<Object, SqlError> {
    let mut st = state.lock().map_err(|_| SqlError::closed_transaction())?;
    if st.finished {
        return Err(SqlError::closed_transaction());
    }
    st.conn.take().ok_or_else(SqlError::closed_transaction)
}

/// Run `op` with the transaction's connection. Fatal (connection-level)
/// errors destroy the connection and poison the transaction; statement-level
/// errors return the connection to the transaction (the transaction is aborted
/// server-side until the guest rolls back, mirroring libpq semantics).
async fn with_txn_conn<T, F, Fut>(
    state: &SharedTxnState,
    destroyed: &Arc<AtomicU64>,
    op: F,
) -> Result<T, SqlError>
where
    F: FnOnce(ConnectionGuard) -> Fut,
    Fut: std::future::Future<Output = (ConnectionGuard, Result<T, sqlx::Error>)>,
{
    let connection = ConnectionGuard::new(take_conn(state)?, Arc::clone(destroyed));
    let (connection, result) = op(connection).await;
    match result {
        Ok(v) => {
            restore_connection(connection, state);
            Ok(v)
        }
        Err(e) => {
            let mapped = map_pg_error(&e);
            if connection_lost(&e) {
                if let Ok(mut st) = state.lock() {
                    st.finished = true;
                }
                drop(connection);
            } else {
                restore_connection(connection, state);
            }
            Err(mapped)
        }
    }
}

async fn finish_statement_txn(
    state: &SharedTxnState,
    destroyed: &Arc<AtomicU64>,
    verb: &str,
) -> Result<(), SqlError> {
    let mut connection = ConnectionGuard::new(take_conn(state)?, Arc::clone(destroyed));
    match super::identity::finish_transaction(connection.connection(), verb).await {
        Ok(()) => {
            if let Ok(mut transaction) = state.lock() {
                transaction.finished = true;
            }
            connection.repool();
            Ok(())
        }
        Err(error) => {
            if let Ok(mut transaction) = state.lock() {
                transaction.finished = true;
            }
            Err(map_pg_error(&error))
        }
    }
}

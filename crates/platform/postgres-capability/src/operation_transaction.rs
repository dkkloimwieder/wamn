//! The transaction that the host owns for one logged operation request element
//! (`docs/history/host-transaction.md` 4.1).
//!
//! The guest borrows a query handle. Only the host commits or rolls back.

use crate::invocation::Authority;
use std::sync::{Arc, Mutex};

use super::resources::{
    PgTransaction, StatementConnectionGuard, prepare_statement_transaction, take_conn,
    with_txn_conn,
};
use super::{PgError, WamnPostgres};

/// The host transaction of one operation request element.
///
/// Every clone names one transaction. When the last clone drops unfinished,
/// the connection is destroyed, so the server aborts the transaction.
#[derive(Debug, Clone)]
pub struct OperationTransaction {
    transaction: Arc<PgTransaction>,
    pub(super) identity: Arc<Mutex<Option<super::session_identity::transactions::BoundIdentity>>>,
    pub(super) finish: Arc<Mutex<Option<[String; 3]>>>,
    pub(super) known_result: Option<String>,
    causation: Option<wamn_event_wire::Causation>,
    /// The declared application that the transaction binds as
    /// `app.application`, read from host state at begin. Numbering resolves
    /// its sequences by it, never by the identity.
    pub(super) application: Option<String>,
}

/// Why a commit of an [`OperationTransaction`] did not succeed.
#[derive(Debug)]
pub enum PgCommitFailure {
    /// The transaction rolled back: the connection was gone before `COMMIT`
    /// was sent, or the server answered `COMMIT` with an error.
    RolledBack(PgError),
    /// `COMMIT` was sent and its answer was not read, so the transaction may
    /// have committed.
    Uncertain(PgError),
}

impl std::fmt::Display for PgCommitFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RolledBack(error) => write!(formatter, "the commit rolled back: {error:?}"),
            Self::Uncertain(error) => {
                write!(formatter, "the commit outcome is unknown: {error:?}")
            }
        }
    }
}

impl std::error::Error for PgCommitFailure {}

impl OperationTransaction {
    /// Store a known success result in the claim, inside this transaction.
    /// A refusal must still roll back the transaction and its claim.
    pub fn with_known_result(mut self, result: String) -> Self {
        self.known_result = Some(result);
        self
    }

    /// A guest handle of this transaction. Dropping it leaves the transaction
    /// open. The SQL interface exposes no completion methods.
    pub(super) async fn lend(&self) -> Result<PgTransaction, PgError> {
        self.start().await?;
        Ok(self.transaction.lent())
    }

    async fn start(&self) -> Result<(), PgError> {
        let identity = self
            .identity
            .lock()
            .expect("request element identity lock poisoned")
            .take();
        if let Some(identity) = identity {
            let mut connection = StatementConnectionGuard::new(
                take_conn(&self.transaction.state)?,
                Arc::clone(&self.transaction.destroyed),
            );
            identity.bind(connection.connection()).await?;
            super::resources::restore_connection(connection, &self.transaction.state);
        }
        Ok(())
    }

    /// Run host statements of the plugin with this transaction's connection,
    /// under its identity, as `resources::with_txn_conn` runs them.
    pub(super) async fn with_host_connection<T, F, Fut>(&self, op: F) -> Result<T, PgError>
    where
        F: FnOnce(StatementConnectionGuard) -> Fut,
        Fut: std::future::Future<Output = (StatementConnectionGuard, Result<T, sqlx::Error>)>,
    {
        self.start().await?;
        with_txn_conn(&self.transaction.state, &self.transaction.destroyed, op).await
    }

    /// The owned transaction, for the host's own statements in it.
    pub(super) fn transaction(&self) -> &PgTransaction {
        &self.transaction
    }

    /// Read the WAL position on the connection of this open transaction.
    pub(super) async fn wal_position(&self) -> Result<String, PgError> {
        self.with_host_connection(|mut connection| async move {
            let result = sqlx::query_scalar("SELECT pg_current_wal_lsn()::text")
                .fetch_one(connection.connection())
                .await;
            (connection, result)
        })
        .await
    }

    /// Commit the transaction.
    ///
    /// # Errors
    ///
    /// Returns [`PgCommitFailure::Uncertain`] only when `COMMIT` was sent and
    /// its answer was not read. Every other failure is a certain rollback.
    pub async fn commit(&self) -> Result<(), PgCommitFailure> {
        self.start().await.map_err(PgCommitFailure::RolledBack)?;
        let state = &self.transaction.state;
        let connection = take_conn(state).map_err(PgCommitFailure::RolledBack)?;
        let mut connection =
            StatementConnectionGuard::new(connection, Arc::clone(&self.transaction.destroyed));
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
        if let Some([key, status, output]) = finish {
            let result = sqlx::query(super::write_log::COMMIT_FINISH_SQL)
                .bind(&key)
                .bind(&status)
                .bind(&output)
                .execute(connection.connection())
                .await;
            if let Err(error) = result {
                mark_finished();
                return Err(PgCommitFailure::RolledBack(super::types::map_pg_error(
                    &error,
                )));
            }
        }
        if let Some(run) = &self.causation {
            let json = serde_json::to_string(run).expect("Causation serializes to JSON");
            if let Err(error) =
                sqlx::query("SELECT pg_logical_emit_message(true, 'wamn.causation', $1::text)")
                    .bind(json)
                    .execute(connection.connection())
                    .await
            {
                mark_finished();
                return Err(PgCommitFailure::RolledBack(super::types::map_pg_error(
                    &error,
                )));
            }
        }
        let result = sqlx::query("COMMIT")
            .execute(connection.connection())
            .await
            .map(drop);
        match result {
            Ok(()) => {
                mark_finished();
                connection.repool();
                Ok(())
            }
            Err(error) => {
                mark_finished();
                let answered = error.as_database_error().is_some();
                drop(connection);
                let error = super::types::map_pg_error(&error);
                Err(if answered {
                    PgCommitFailure::RolledBack(error)
                } else {
                    PgCommitFailure::Uncertain(error)
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
    pub async fn rollback(&self) -> Result<(), PgError> {
        if self
            .transaction
            .state
            .lock()
            .is_ok_and(|state| state.finished && state.conn.is_none())
        {
            return Ok(());
        }
        match super::resources::finish_statement_txn(
            &self.transaction.state,
            &self.transaction.destroyed,
            "ROLLBACK",
        )
        .await
        {
            Err(PgError::ConnectionUnavailable) => Ok(()),
            finished => finished,
        }
    }
}

impl WamnPostgres {
    /// Begin the host transaction of one operation request element under the identity that
    /// `scope` binds.
    ///
    /// # Errors
    ///
    /// Returns the error of checkout or binding the transaction identity.
    pub async fn begin_operation_transaction(
        &self,
        scope: &str,
    ) -> Result<OperationTransaction, PgError> {
        let project = self.project_for(scope);
        let (transaction, identity) = prepare_statement_transaction(self, scope, &project).await?;
        let transaction = OperationTransaction {
            transaction: Arc::new(transaction),
            identity: Arc::new(Mutex::new(Some(identity))),
            finish: Arc::new(Mutex::new(None)),
            known_result: None,
            causation: if self.active_read(scope) {
                None
            } else {
                self.current_run_for(scope)
            },
            application: self.application_for(scope),
        };
        transaction.start().await?;
        Ok(transaction)
    }

    /// Whether the active operation uses a read transaction.
    pub(super) fn active_read(&self, scope: &str) -> bool {
        self.scopes
            .read::<Authority, _>(scope, |record| record.read)
            .unwrap_or(false)
    }

    /// Bind `transaction` as the operation transaction of `scope`.
    ///
    /// # Errors
    ///
    /// Fails when `scope` already has one.
    pub fn bind_operation_transaction(
        &self,
        scope: &str,
        transaction: &OperationTransaction,
    ) -> anyhow::Result<()> {
        self.scopes
            .update_existing::<Authority, _>(scope, |record| {
                anyhow::ensure!(
                    record.transaction.is_none(),
                    "operation-transaction-scope-already-bound"
                );
                record.transaction = Some(transaction.clone());
                Ok(())
            })
            .unwrap_or_else(|| Err(anyhow::anyhow!("invocation scope was revoked")))
    }

    /// Drop the binding of `scope`. The transaction stays open for its owner.
    pub fn revoke_operation_transaction(&self, scope: &str) {
        self.scopes
            .update_existing::<Authority, _>(scope, |record| record.transaction = None);
    }

    pub(super) fn operation_transaction_for(&self, scope: &str) -> Option<OperationTransaction> {
        self.scopes
            .read::<Authority, _>(scope, |record| record.transaction.clone())
            .flatten()
    }
}

#[cfg(test)]
mod tests;

//! The transaction that the host owns for one logged operation request element
//! (`docs/history/host-transaction.md` 4.1).
//!
//! The guest borrows a query handle and must leave completion to the host.

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
    causation: Option<wamn_event::Causation>,
}

/// Why a commit of an [`OperationTransaction`] did not succeed.
#[derive(Debug)]
pub enum PgCommitFailure {
    /// The transaction rolled back: the connection was gone before `COMMIT`
    /// was sent, or the server answered `COMMIT` with an error.
    RolledBack(PgError),
    /// The guest ended the host transaction, or `COMMIT` was sent and its
    /// answer was not read. Some work may have committed.
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
    /// Stage a known success result for the host commit.
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

    pub(super) async fn start(&self) -> Result<(), PgError> {
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
            let xid = identity.bind(connection.connection()).await?;
            self.transaction
                .state
                .lock()
                .expect("transaction lock poisoned")
                .xid = Some(xid);
            super::resources::restore_connection(connection, &self.transaction.state);
        }
        Ok(())
    }

    /// Run host SQL with this transaction's connection and caller identity.
    pub async fn with_host_connection<T, F, Fut>(&self, op: F) -> Result<T, PgError>
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
    /// Returns [`PgCommitFailure::Uncertain`] when the guest ended the host
    /// transaction or the host did not read the answer to `COMMIT`.
    pub async fn commit(&self) -> Result<(), PgCommitFailure> {
        self.start().await.map_err(PgCommitFailure::RolledBack)?;
        let state = &self.transaction.state;
        let xid = state
            .lock()
            .expect("transaction lock poisoned")
            .xid
            .as_ref()
            .expect("host identity was inserted")
            .clone();
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
        let result = super::write_log::commit_transaction(
            connection.connection(),
            &xid,
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
                let error = super::types::map_pg_error(&error);
                Err(if answered && !ended_early {
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

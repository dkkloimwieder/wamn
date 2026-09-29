//! The transaction that the host owns for one logged operation item
//! (`docs/plan/host-transaction.md` 4.1).
//!
//! The host begins it before the guest call, under the claims of the call, in
//! the same `BEGIN` batch as every statement transaction. The guest works in
//! it through `statements.operation-transaction`, and its `begin` refuses
//! while it is bound. After the call the host commits or rolls it back.

use std::collections::HashMap;
use std::sync::Arc;

use super::resources::{
    PgTransaction, StatementConnectionGuard, begin_statement_transaction, take_conn,
};
use super::{PgError, WamnPostgres};

/// The SQLSTATE of a `begin` inside an operation that has a host transaction:
/// `active_sql_transaction`.
pub(super) const BEGIN_REFUSED: &str = "25001";
/// The SQLSTATE of a guest `commit` or `rollback` of the host transaction:
/// `invalid_transaction_termination`.
pub(super) const FINISH_REFUSED: &str = "2D000";

/// The host transaction of one operation item.
///
/// Every clone names one transaction. When the last clone drops unfinished,
/// the connection is destroyed, so the server aborts the transaction.
#[derive(Debug, Clone)]
pub struct OperationTransaction {
    transaction: Arc<PgTransaction>,
}

/// Why a commit of an [`OperationTransaction`] did not succeed.
#[derive(Debug)]
pub enum CommitFailure {
    /// The transaction rolled back: the connection was gone before `COMMIT`
    /// was sent, or the server answered `COMMIT` with an error.
    RolledBack(PgError),
    /// `COMMIT` was sent and its answer was not read, so the transaction may
    /// have committed.
    Uncertain(PgError),
}

impl std::fmt::Display for CommitFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RolledBack(error) => write!(formatter, "the commit rolled back: {error:?}"),
            Self::Uncertain(error) => {
                write!(formatter, "the commit outcome is unknown: {error:?}")
            }
        }
    }
}

impl std::error::Error for CommitFailure {}

impl OperationTransaction {
    /// A guest handle of this transaction. Dropping it leaves the transaction
    /// open, and it refuses `commit` and `rollback`.
    pub(super) fn lend(&self) -> PgTransaction {
        self.transaction.lent()
    }

    /// The owned transaction, for the host's own statements in it.
    pub(super) fn transaction(&self) -> &PgTransaction {
        &self.transaction
    }

    /// Commit the transaction.
    ///
    /// # Errors
    ///
    /// Returns [`CommitFailure::Uncertain`] only when `COMMIT` was sent and
    /// its answer was not read. Every other failure is a certain rollback.
    pub async fn commit(&self) -> Result<(), CommitFailure> {
        let state = &self.transaction.state;
        super::transaction_views::finish_view(state).await;
        let connection = take_conn(state).map_err(CommitFailure::RolledBack)?;
        let connection =
            StatementConnectionGuard::new(connection, Arc::clone(&self.transaction.destroyed));
        let mark_finished = || {
            if let Ok(mut transaction) = state.lock() {
                transaction.finished = true;
            }
        };
        if connection.connection().is_closed() {
            mark_finished();
            return Err(CommitFailure::RolledBack(PgError::ConnectionUnavailable));
        }
        match connection.connection().batch_execute("COMMIT").await {
            Ok(()) => {
                mark_finished();
                connection.repool();
                Ok(())
            }
            Err(error) => {
                mark_finished();
                let answered = error.as_db_error().is_some();
                drop(connection);
                let error = super::types::map_pg_error(&error);
                Err(if answered {
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

impl wamn_engine::operation::ItemTransaction for OperationTransaction {
    async fn commit(&self) -> Result<(), wamn_engine::operation::ItemCommitFailure> {
        use wamn_engine::operation::ItemCommitFailure;
        // The generated error literals of a failed write log commit.
        let code = |error: &PgError| match error {
            PgError::SerializationFailure | PgError::ConnectionUnavailable => "retry",
            PgError::StatementTimeout => "timeout",
            _ => "internal_error",
        };
        OperationTransaction::commit(self)
            .await
            .map_err(|failure| match failure {
                CommitFailure::RolledBack(error) => ItemCommitFailure::RolledBack {
                    code: code(&error),
                    message: format!("the commit rolled back: {error:?}"),
                },
                CommitFailure::Uncertain(error) => ItemCommitFailure::Uncertain {
                    message: format!("the commit outcome is unknown: {error:?}"),
                },
            })
    }

    async fn rollback(&self) -> anyhow::Result<()> {
        OperationTransaction::rollback(self)
            .await
            .map_err(|error| anyhow::anyhow!("roll back the item transaction: {error:?}"))
    }

    /// The item claims in the write log, inside this transaction.
    fn intent_store(&self) -> Option<&dyn wamn_run_state::IntentStore> {
        Some(self)
    }
}

/// The operation transactions bound to invocation scopes.
pub(super) type OperationTransactions = HashMap<String, OperationTransaction>;

impl WamnPostgres {
    /// Begin the host transaction of one operation item under the claims that
    /// `scope` binds.
    ///
    /// # Errors
    ///
    /// Returns the error of the checkout or of the `BEGIN` batch.
    pub async fn begin_operation_transaction(
        &self,
        scope: &str,
    ) -> Result<OperationTransaction, PgError> {
        let project = self.project_for(scope);
        let transaction = begin_statement_transaction(self, scope, &project).await?;
        Ok(OperationTransaction {
            transaction: Arc::new(transaction),
        })
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
        let mut bound = self
            .operation_transactions
            .lock()
            .expect("operation transactions lock poisoned");
        anyhow::ensure!(
            !bound.contains_key(scope),
            "operation-transaction-scope-already-bound"
        );
        bound.insert(scope.to_owned(), transaction.clone());
        Ok(())
    }

    /// Drop the binding of `scope`. The transaction stays open for its owner.
    pub fn revoke_operation_transaction(&self, scope: &str) {
        self.operation_transactions
            .lock()
            .expect("operation transactions lock poisoned")
            .remove(scope);
    }

    pub(super) fn operation_transaction_for(&self, scope: &str) -> Option<OperationTransaction> {
        self.operation_transactions
            .lock()
            .expect("operation transactions lock poisoned")
            .get(scope)
            .cloned()
    }
}

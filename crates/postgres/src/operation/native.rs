//! Authorized Postgres transactions for native callers.

use std::sync::Arc;

use anyhow::Context as _;
use wamn_connection::administration_transaction::{
    AdministrationOutcome, AdministrationSession, AdministrationTransactions,
};
use wamn_identity_protocol::{AuthorityClass, PrincipalId};
use wamn_package::canonical_name::CanonicalName;
use wamn_registry::{ProjectEnvKey, ProjectId};
use wamn_run_state::run_store::ProductionLeaseError;

use crate::WamnPostgres;
use crate::connection_guard::ConnectionGuard;
use crate::executor::{open_executor_transaction, run_executor_transaction};
use crate::run_store::{executor_commit_error, executor_open_error};
use crate::transaction_identity::{TransactionIdentity, prepare_identity};

use super::identity::finish_transaction;

/// Serialize role and permission writes for the project environment.
///
/// # Errors
/// Returns a database error if the transaction cannot acquire the lock.
pub async fn lock_operation_grants(
    connection: &mut sqlx::PgConnection,
    key: &ProjectEnvKey,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
     'wamn.operation-grants:' || $1, 0))",
    )
    .bind(key.as_str())
    .fetch_one(connection)
    .await?;
    Ok(())
}

impl WamnPostgres {
    /// Write through the executor pool in one transaction. The callback must not end it.
    pub async fn executor_write<T, E>(
        &self,
        project: &wamn_registry::ProjectId,
        write: impl AsyncFnOnce(&mut sqlx::PgConnection) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<ProductionLeaseError>,
    {
        let connection = open_executor_transaction(self, project, Some("wamn_run"), false)
            .await
            .map_err(|error| E::from(executor_open_error(error)))?;
        run_executor_transaction(connection, write, |error| {
            E::from(executor_commit_error(error))
        })
        .await
    }
}

impl AdministrationSession for ConnectionGuard {
    type Connection = sqlx::PgConnection;

    fn connection(&mut self) -> &mut Self::Connection {
        self.connection()
    }

    async fn finish(mut self, outcome: AdministrationOutcome) -> anyhow::Result<()> {
        let end = match outcome {
            AdministrationOutcome::Commit => "COMMIT",
            AdministrationOutcome::Rollback => "ROLLBACK",
        };
        finish_transaction(self.connection(), end)
            .await
            .context("end the host route transaction")?;
        self.repool();
        Ok(())
    }

    fn repool(self) {
        self.repool();
    }
}

impl AdministrationTransactions for WamnPostgres {
    type Session = ConnectionGuard;

    async fn begin_administration(
        &self,
        project: &ProjectId,
        principal_id: &PrincipalId,
        operation: &CanonicalName,
    ) -> anyhow::Result<Self::Session> {
        let (connection, policy) = self
            .checkout_platform(project, AuthorityClass::Platform)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let mut connection =
            ConnectionGuard::new(connection, Arc::clone(self.destroyed_connections()));
        let identity = TransactionIdentity {
            principal: Some(principal_id),
            operation: Some(operation),
            ..TransactionIdentity::default()
        };
        prepare_identity(
            AuthorityClass::Platform,
            false,
            &identity,
            policy.statement_timeout_ms,
        )
        .context("begin the host route transaction")?
        .bind(connection.connection())
        .await
        .context("begin the host route transaction")?;
        Ok(connection)
    }
}

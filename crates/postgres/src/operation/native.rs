//! Authorized Postgres transactions for native callers.

use std::sync::Arc;

use anyhow::Context as _;
use wamn_connection::administration_transaction::{
    AdministrationOutcome, AdministrationSession, AdministrationTransactions,
};
use wamn_identity_protocol::{AuthorityClass, PlatformComponent, PrincipalId};
use wamn_package::canonical_name::CanonicalName;
use wamn_registry::{ProjectEnvKey, ProjectId};
use wamn_run_state::run_store::ProductionLeaseError;

use crate::WamnPostgres;
use crate::run_store::postgres_error;

use super::ConnectionGuard;
use super::identity::{TransactionIdentity, finish_transaction, prepare_identity};

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
    /// Read through the authorized executor pool.
    pub async fn executor_read<T>(
        &self,
        project: &wamn_registry::ProjectId,
        read: impl AsyncFnOnce(&mut sqlx::PgConnection) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let connection = open_executor_read(self, project).await?;
        run_executor_transaction(connection, read).await
    }

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
            .map_err(E::from)?;
        run_executor_transaction(connection, write).await
    }
}

async fn run_executor_transaction<T, E>(
    mut connection: ConnectionGuard,
    work: impl AsyncFnOnce(&mut sqlx::PgConnection) -> Result<T, E>,
) -> Result<T, E>
where
    E: From<ProductionLeaseError>,
{
    let result = work(connection.connection()).await;
    match result {
        Ok(value) => {
            sqlx::raw_sql("COMMIT")
                .execute(connection.connection())
                .await
                .map_err(|error| E::from(postgres_error("commit executor transaction", &error)))?;
            connection.repool();
            Ok(value)
        }
        Err(error) => {
            if sqlx::raw_sql("ROLLBACK")
                .execute(connection.connection())
                .await
                .is_ok()
            {
                connection.repool();
            }
            Err(error)
        }
    }
}

async fn open_executor_transaction(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
    schema: Option<&str>,
    read_only: bool,
) -> Result<ConnectionGuard, ProductionLeaseError> {
    let (connection, policy) = postgres
        .checkout_platform(project, AuthorityClass::Platform)
        .await
        .map_err(|error| ProductionLeaseError::storage("checkout executor connection", error))?;
    // Cancellation destroys a connection whose transaction has not ended.
    let mut connection =
        ConnectionGuard::new(connection, Arc::clone(postgres.destroyed_connections()));
    let principal = PlatformComponent::Executor.principal_id();
    let operation = PlatformComponent::Executor
        .principal_name()
        .parse()
        .expect("platform operation");
    let identity = TransactionIdentity {
        schema,
        principal: Some(&principal),
        operation: Some(&operation),
        ..TransactionIdentity::default()
    };
    prepare_identity(
        AuthorityClass::Platform,
        read_only,
        &identity,
        policy.statement_timeout_ms,
    )
    .map_err(|error| ProductionLeaseError::storage("begin executor transaction", error))?
    .bind(connection.connection())
    .await
    .map_err(|error| ProductionLeaseError::storage("begin executor transaction", error))?;
    Ok(connection)
}

pub async fn open_executor_read(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
) -> Result<ConnectionGuard, ProductionLeaseError> {
    open_executor_transaction(postgres, project, None, true).await
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

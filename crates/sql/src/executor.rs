//! Authorized executor transactions shared by reads and run persistence.

use super::{WamnPostgres, resources::StatementConnectionGuard};
use std::sync::Arc;
use wamn_identity_protocol::{AuthorityClass, PlatformComponent};
use wamn_run_state::run_store::{ProductionLeaseError, ProductionLeaseErrorType};

/// Host identity for one executor transaction.
struct ExecutorIdentity<'a> {
    schema: Option<&'a str>,
    runner: Option<&'a str>,
    user: Option<&'a wamn_identity_protocol::PrincipalId>,
    operation: Option<&'a wamn_catalog::canonical_name::CanonicalName>,
    application: Option<&'a wamn_catalog::ApplicationName>,
}

impl WamnPostgres {
    /// Read through the authorized executor pool.
    pub async fn executor_read<T>(
        &self,
        project: &wamn_registry::ProjectId,
        read: impl AsyncFnOnce(&mut sqlx::PgConnection) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        self.executor_transaction(
            project,
            ExecutorIdentity {
                schema: None,
                runner: None,
                user: None,
                operation: None,
                application: None,
            },
            true,
            read,
        )
        .await
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
        let executor = PlatformComponent::Executor.principal_id();
        let operation: wamn_catalog::canonical_name::CanonicalName = PlatformComponent::Executor
            .principal_name()
            .parse()
            .expect("platform operation");
        self.executor_transaction(
            project,
            ExecutorIdentity {
                schema: Some("wamn_run"),
                runner: None,
                user: Some(&executor),
                operation: Some(&operation),
                application: None,
            },
            false,
            write,
        )
        .await
    }

    async fn executor_transaction<T, E>(
        &self,
        project: &wamn_registry::ProjectId,
        identity: ExecutorIdentity<'_>,
        read_only: bool,
        work: impl AsyncFnOnce(&mut sqlx::PgConnection) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<ProductionLeaseError>,
    {
        let mut connection = self
            .open_executor_transaction(project, identity, read_only)
            .await
            .map_err(E::from)?;
        let result = work(connection.connection()).await;
        match result {
            Ok(value) => {
                sqlx::raw_sql("COMMIT")
                    .execute(connection.connection())
                    .await
                    .map_err(|error| {
                        E::from(ProductionLeaseError::postgres(
                            "commit executor transaction",
                            &error,
                        ))
                    })?;
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
        &self,
        project: &wamn_registry::ProjectId,
        identity: ExecutorIdentity<'_>,
        read_only: bool,
    ) -> Result<StatementConnectionGuard, ProductionLeaseError> {
        let failure = |operation, error| {
            ProductionLeaseError::with_source(ProductionLeaseErrorType::Storage, operation, error)
        };
        let (connection, policy) = self
            .checkout_platform(project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| failure("checkout executor connection", error))?;
        // Cancellation destroys a connection whose transaction has not ended.
        let mut connection = StatementConnectionGuard::new(connection, Arc::clone(&self.destroyed));
        let principal = identity
            .user
            .copied()
            .unwrap_or_else(|| PlatformComponent::Executor.principal_id());
        self.begin_with_identity(
            connection.connection(),
            AuthorityClass::ExecutorPlatform,
            read_only,
            identity.schema,
            identity.runner,
            None,
            Some(&principal),
            Some(
                identity
                    .operation
                    .unwrap_or(&"wamn:executor".parse().expect("platform operation")),
            ),
            identity.application,
            None,
            policy.statement_timeout_ms,
        )
        .await
        .map_err(|error| failure("begin executor transaction", error))?;
        Ok(connection)
    }
}

#[async_trait::async_trait]
impl wamn_connection::database::ProjectDatabase for WamnPostgres {
    async fn read_connection(
        &self,
        project: &wamn_registry::ProjectId,
    ) -> anyhow::Result<StatementConnectionGuard> {
        self.open_executor_transaction(
            project,
            ExecutorIdentity {
                schema: None,
                runner: None,
                user: None,
                operation: None,
                application: None,
            },
            true,
        )
        .await
        .map_err(Into::into)
    }
}

impl WamnPostgres {
    /// Check out the application's existing SQL pool without starting a transaction.
    pub async fn application_connection(
        &self,
        project: &wamn_registry::ProjectId,
        project_env_key: &wamn_registry::ProjectEnvKey,
    ) -> anyhow::Result<sqlx::pool::PoolConnection<sqlx::Postgres>> {
        let (connection, _) = self
            .checkout_guest(project, project_env_key)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(connection)
    }
}

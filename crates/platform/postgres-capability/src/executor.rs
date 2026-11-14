//! Authorized executor transactions shared by reads and run persistence.

use super::{WamnPostgres, resources::StatementConnectionGuard};
use std::sync::Arc;
use wamn_identity_protocol::{AuthorityClass, PlatformComponent};
use wamn_run_state::run_store::{ProductionLeaseError, ProductionLeaseErrorType};

/// Host identity for one executor transaction.
struct ExecutorIdentity<'a> {
    project_env_key: &'a str,
    schema: Option<&'a str>,
    runner: Option<&'a str>,
    user: Option<&'a str>,
    operation: Option<&'a str>,
    application: Option<&'a str>,
}

impl WamnPostgres {
    /// Read through the authorized executor pool.
    pub async fn executor_read<T>(
        &self,
        project: &str,
        project_env_key: &str,
        read: impl AsyncFnOnce(&mut sqlx::PgConnection) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        self.executor_transaction(
            project,
            ExecutorIdentity {
                project_env_key,
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
        project: &str,
        project_env_key: &str,
        write: impl AsyncFnOnce(&mut sqlx::PgConnection) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<ProductionLeaseError>,
    {
        let executor = PlatformComponent::Executor.principal_id().to_string();
        let operation = PlatformComponent::Executor.principal_name();
        self.executor_transaction(
            project,
            ExecutorIdentity {
                project_env_key,
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

    pub(super) async fn executor_bound<T>(
        &self,
        component: &str,
        work: impl AsyncFnOnce(&mut sqlx::PgConnection) -> Result<T, ProductionLeaseError>,
    ) -> Result<T, ProductionLeaseError> {
        let project_env_key = self.project_env_key_for(component).ok_or_else(|| {
            ProductionLeaseError::new(
                ProductionLeaseErrorType::Identity,
                "resolve executor identity",
                "component has no host-injected project",
            )
        })?;
        let project = self.project_for(component);
        let schema = self.schema_for(component);
        let runner = self.runner_for(component);
        let user = self.user_id_for(component);
        let operation = self.operation_for(component);
        let application = self.application_for(component);
        self.executor_transaction(
            &project,
            ExecutorIdentity {
                project_env_key: &project_env_key,
                schema: schema.as_deref(),
                runner: runner.as_deref(),
                user: user.as_deref(),
                operation: operation.as_deref(),
                application: application.as_deref(),
            },
            false,
            work,
        )
        .await
    }

    async fn executor_transaction<T, E>(
        &self,
        project: &str,
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
        project: &str,
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
        self.begin_with_identity(
            connection.connection(),
            AuthorityClass::ExecutorPlatform,
            read_only,
            identity.project_env_key,
            identity.schema,
            identity.runner,
            None,
            identity.user,
            identity.operation,
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
impl wamn_connections::database::ProjectDatabase for WamnPostgres {
    async fn read_connection(
        &self,
        project: &str,
        project_env_key: &str,
    ) -> anyhow::Result<StatementConnectionGuard> {
        self.open_executor_transaction(
            project,
            ExecutorIdentity {
                project_env_key,
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

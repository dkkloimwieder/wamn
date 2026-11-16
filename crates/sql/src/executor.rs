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
    operation: Option<&'a str>,
    application: Option<&'a wamn_catalog::ApplicationName>,
}

impl WamnPostgres {
    /// Read through the authorized executor pool.
    pub async fn executor_read<T>(
        &self,
        project: &str,
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
        project: &str,
        write: impl AsyncFnOnce(&mut sqlx::PgConnection) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<ProductionLeaseError>,
    {
        let executor = PlatformComponent::Executor.principal_id();
        let operation = PlatformComponent::Executor.principal_name();
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
            Some(identity.operation.unwrap_or("wamn:executor")),
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
    async fn read_connection(&self, project: &str) -> anyhow::Result<StatementConnectionGuard> {
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
    /// Whether a released invocation still retains this deployment.
    pub fn release_in_use(&self, release: &wamn_engine::release_manifest::LoadedRelease) -> bool {
        self.scopes
            .any::<wamn_connections::invocation::Scope>(|record| {
                record.invocation.is_some()
                    && record
                        .context
                        .as_ref()
                        .is_some_and(|context| std::ptr::eq(context.release.as_ref(), release))
            })
    }
}

impl wamn_connections::production_lease::ExecutorDatabase for WamnPostgres {
    async fn bound_connection(
        &self,
        component: &str,
    ) -> Result<StatementConnectionGuard, ProductionLeaseError> {
        self.project_env_key_for(component).ok_or_else(|| {
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
        self.open_executor_transaction(
            &project,
            ExecutorIdentity {
                schema: schema.as_deref(),
                runner: runner.as_deref(),
                user: user.as_ref(),
                operation: operation.as_deref(),
                application: application.as_ref(),
            },
            false,
        )
        .await
    }
    fn project_env_key(&self, component: &str) -> Option<wamn_registry::ProjectEnvKey> {
        self.project_env_key_for(component)
    }
    fn runner(&self, component: &str) -> Option<String> {
        self.runner_for(component)
    }
    fn manifest_digest(&self, component: &str) -> Option<wamn_digest::Digest> {
        self.release_identity_for(component)
            .map(|release| *release.manifest_digest())
    }
}

impl WamnPostgres {
    /// Check out the application's existing SQL pool without starting a transaction.
    pub async fn application_connection(
        &self,
        project: &str,
        project_env_key: &wamn_registry::ProjectEnvKey,
    ) -> anyhow::Result<sqlx::pool::PoolConnection<sqlx::Postgres>> {
        let (connection, _) = self
            .checkout_guest(project, project_env_key)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(connection)
    }
}

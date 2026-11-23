//! Authorized executor transactions shared by reads and run persistence.

use std::sync::Arc;

use wamn_identity_protocol::{AuthorityClass, PlatformComponent};
use wamn_postgres::WamnPostgres;
use wamn_run_state::run_store::ProductionLeaseError;

use super::{OperationHost, resources::StatementConnectionGuard, transactions};

/// Host identity for one executor transaction.
struct ExecutorIdentity<'a> {
    schema: Option<&'a str>,
    runner: Option<&'a str>,
    user: Option<&'a wamn_identity_protocol::PrincipalId>,
    operation: Option<&'a wamn_package::canonical_name::CanonicalName>,
    application: Option<&'a wamn_registry::ApplicationName>,
}

impl OperationHost {
    /// Read through the authorized executor pool.
    pub async fn executor_read<T>(
        &self,
        project: &wamn_registry::ProjectId,
        read: impl AsyncFnOnce(&mut sqlx::PgConnection) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let connection = open_executor_read(&self.pools, project).await?;
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
        let executor = PlatformComponent::Executor.principal_id();
        let operation: wamn_package::canonical_name::CanonicalName = PlatformComponent::Executor
            .principal_name()
            .parse()
            .expect("platform operation");
        let connection = open_executor_transaction(
            &self.pools,
            project,
            ExecutorIdentity {
                schema: Some("wamn_run"),
                runner: None,
                user: Some(&executor),
                operation: Some(&operation),
                application: None,
            },
            false,
        )
        .await
        .map_err(E::from)?;
        run_executor_transaction(connection, write).await
    }
}

async fn run_executor_transaction<T, E>(
    mut connection: StatementConnectionGuard,
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
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
    identity: ExecutorIdentity<'_>,
    read_only: bool,
) -> Result<StatementConnectionGuard, ProductionLeaseError> {
    let (connection, policy) = postgres
        .checkout_platform(project, AuthorityClass::Platform)
        .await
        .map_err(|error| ProductionLeaseError::storage("checkout executor connection", error))?;
    // Cancellation destroys a connection whose transaction has not ended.
    let mut connection =
        StatementConnectionGuard::new(connection, Arc::clone(postgres.destroyed_connections()));
    let principal = identity
        .user
        .copied()
        .unwrap_or_else(|| PlatformComponent::Executor.principal_id());
    transactions::begin_with_identity(
        connection.connection(),
        AuthorityClass::Platform,
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
    .map_err(|error| ProductionLeaseError::storage("begin executor transaction", error))?;
    Ok(connection)
}

async fn open_executor_read(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
) -> Result<StatementConnectionGuard, ProductionLeaseError> {
    open_executor_transaction(
        postgres,
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
}

/// Connection facts use the same executor read without retaining the operation plugin.
#[derive(Debug)]
pub struct OperationDatabase {
    pools: Arc<WamnPostgres>,
}

impl OperationDatabase {
    /// Use the same project pools as the operation host.
    pub fn new(pools: Arc<WamnPostgres>) -> Self {
        Self { pools }
    }
}

#[async_trait::async_trait]
impl wamn_connection::database::ProjectDatabase for OperationDatabase {
    async fn oauth_connection(
        &self,
        project: &wamn_registry::ProjectId,
    ) -> anyhow::Result<sqlx::pool::PoolConnection<sqlx::Postgres>> {
        wamn_authentication::authorization::oauth_connection(&self.pools, project).await
    }

    async fn read_connection(
        &self,
        project: &wamn_registry::ProjectId,
    ) -> anyhow::Result<StatementConnectionGuard> {
        Ok(open_executor_read(&self.pools, project).await?)
    }
}

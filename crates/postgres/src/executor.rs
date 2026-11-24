//! Authorized connections for executor transactions.

use std::{error::Error, fmt, sync::Arc};

use wamn_identity_protocol::{AuthorityClass, PlatformComponent};

use crate::WamnPostgres;
use crate::connection_guard::ConnectionGuard;
use crate::postgres_error_detail;
use crate::transaction_identity::{TransactionIdentity, prepare_identity};

/// Failure to check out or start an executor transaction.
#[derive(Debug)]
pub struct ExecutorOpenError {
    operation: &'static str,
    source: Box<dyn Error + Send + Sync>,
}

impl ExecutorOpenError {
    fn new(operation: &'static str, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            operation,
            source: Box::new(source),
        }
    }

    /// Return the failed operation and its native error for boundary translation.
    pub fn into_parts(self) -> (&'static str, Box<dyn Error + Send + Sync>) {
        (self.operation, self.source)
    }

    /// Report an executor read failure with its native error as the source.
    pub fn into_read_error(self) -> anyhow::Error {
        ExecutorReadError {
            operation: self.operation,
            detail: self.source.to_string(),
            source: Some(self.source),
        }
        .into()
    }
}

impl fmt::Display for ExecutorOpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} failed", self.operation)
    }
}

impl Error for ExecutorOpenError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug)]
struct ExecutorReadError {
    operation: &'static str,
    detail: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl fmt::Display for ExecutorReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "production lease {} failed: {}",
            self.operation, self.detail
        )
    }
}

impl Error for ExecutorReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

impl WamnPostgres {
    /// Read through the authorized executor pool.
    pub async fn executor_read<T>(
        &self,
        project: &wamn_registry::ProjectId,
        read: impl AsyncFnOnce(&mut sqlx::PgConnection) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let connection = open_executor_read(self, project)
            .await
            .map_err(ExecutorOpenError::into_read_error)?;
        run_executor_transaction(connection, read, |error| {
            ExecutorReadError {
                operation: "commit executor transaction",
                detail: postgres_error_detail(error),
                source: None,
            }
            .into()
        })
        .await
    }
}

/// Run work on the guarded connection and settle the executor transaction.
pub async fn run_executor_transaction<T, E>(
    mut connection: ConnectionGuard,
    work: impl AsyncFnOnce(&mut sqlx::PgConnection) -> Result<T, E>,
    commit_error: impl FnOnce(&sqlx::Error) -> E,
) -> Result<T, E> {
    let result = work(connection.connection()).await;
    match result {
        Ok(value) => {
            sqlx::raw_sql("COMMIT")
                .execute(connection.connection())
                .await
                .map_err(|error| commit_error(&error))?;
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

/// Check out a platform connection and bind executor identity before use.
pub async fn open_executor_transaction(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
    schema: Option<&str>,
    read_only: bool,
) -> Result<ConnectionGuard, ExecutorOpenError> {
    let (connection, policy) = postgres
        .checkout_platform(project, AuthorityClass::Platform)
        .await
        .map_err(|error| ExecutorOpenError::new("checkout executor connection", error))?;
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
    .map_err(|error| ExecutorOpenError::new("begin executor transaction", error))?
    .bind(connection.connection())
    .await
    .map_err(|error| ExecutorOpenError::new("begin executor transaction", error))?;
    Ok(connection)
}

/// Open an executor transaction with a read-only snapshot.
pub async fn open_executor_read(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
) -> Result<ConnectionGuard, ExecutorOpenError> {
    open_executor_transaction(postgres, project, None, true).await
}

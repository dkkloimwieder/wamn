//! Authorized Postgres transactions for native callers.

use std::sync::Arc;

use anyhow::Context as _;
use wamn_identity_protocol::{AuthorityClass, PrincipalId};
use wamn_package::canonical_name::CanonicalName;
use wamn_postgres::{
    ConnectionGuard, TransactionIdentity, WamnPostgres, open_executor_transaction,
    prepare_identity, run_executor_transaction,
};
use wamn_registry::ProjectId;

use crate::run_store::{ProductionLeaseError, executor_commit_error, executor_open_error};

use super::identity::finish_transaction;

/// Write through the executor pool in one transaction. The callback must not end it.
pub async fn executor_write<T, E>(
    postgres: &WamnPostgres,
    project: &wamn_registry::ProjectId,
    write: impl AsyncFnOnce(&mut sqlx::PgConnection) -> Result<T, E>,
) -> Result<T, E>
where
    E: From<ProductionLeaseError>,
{
    let connection = open_executor_transaction(postgres, project, Some("wamn_run"), false)
        .await
        .map_err(|error| E::from(executor_open_error(error)))?;
    run_executor_transaction(connection, write, |error| {
        E::from(executor_commit_error(error))
    })
    .await
}

/// Open an administration transaction whose caller owns settlement and any session locks.
pub async fn begin_administration(
    postgres: &WamnPostgres,
    project: &ProjectId,
    principal_id: &PrincipalId,
    operation: &CanonicalName,
) -> anyhow::Result<ConnectionGuard> {
    let (connection, policy) = postgres
        .checkout_platform(project, AuthorityClass::Platform)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let mut connection =
        ConnectionGuard::new(connection, Arc::clone(postgres.destroyed_connections()));
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

/// Run the callback with the caller’s authority and settle its transaction.
///
/// # Errors
/// Returns a session error if opening or settling the transaction fails.
pub async fn administration_transaction<T, E>(
    postgres: &WamnPostgres,
    project: &ProjectId,
    principal: &PrincipalId,
    operation: &CanonicalName,
    work: impl AsyncFnOnce(&mut sqlx::PgConnection) -> Result<T, E>,
) -> anyhow::Result<Result<T, E>> {
    let mut session = begin_administration(postgres, project, principal, operation).await?;
    let result = work(session.connection()).await;
    let end = if result.is_ok() { "COMMIT" } else { "ROLLBACK" };
    finish_transaction(session.connection(), end)
        .await
        .context("end the host route transaction")?;
    session.repool();
    Ok(result)
}

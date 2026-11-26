//! PostgreSQL implementation of authorized application database operations.

mod identity;
mod native;
mod query;
mod transaction;
mod values;
mod write_log;

use std::sync::Arc;

use wamn_app::database::{Claim, DatabaseTarget, SqlError, SqlRows, SqlValue};
use wamn_app::session_identity::SessionIdentity;
use wamn_event::Causation;
use wamn_identity_protocol::AuthorityClass;
use wamn_package::canonical_name::CanonicalName;

pub use native::{administration_transaction, begin_administration, executor_write};
use query::{QueryTransaction, open_stream};
pub(crate) use transaction::Transaction;
use values::checkout_pg_error;
use wamn_postgres::{ConnectionGuard, WamnPostgres, prepare_identity};
use write_log::WriteLog;

pub(crate) async fn read(
    postgres: &WamnPostgres,
    target: DatabaseTarget<'_>,
    identity: &SessionIdentity,
    sql: String,
    params: Vec<SqlValue>,
) -> Result<SqlRows, SqlError> {
    let (connection, policy) = postgres
        .checkout_dynamic(target.project, target.env)
        .await
        .map_err(checkout_pg_error)?;
    let mut connection =
        ConnectionGuard::new(connection, Arc::clone(postgres.destroyed_connections()));
    let xid = prepare_identity(
        AuthorityClass::DynamicSql,
        true,
        &identity.into(),
        policy.statement_timeout_ms,
    )?
    .bind(connection.connection())
    .await?;
    open_stream(connection, QueryTransaction::Dynamic(xid), sql, params).await
}

pub(crate) async fn begin(
    postgres: &WamnPostgres,
    scope: &str,
    target: DatabaseTarget<'_>,
    identity: &SessionIdentity,
    read: bool,
    causation: Option<Causation>,
) -> Result<Arc<Transaction>, SqlError> {
    let (connection, policy, authority) = postgres
        .checkout_workload(scope, target.project, target.env)
        .await
        .map_err(checkout_pg_error)?;
    let mut connection =
        ConnectionGuard::new(connection, Arc::clone(postgres.destroyed_connections()));
    let bound = prepare_identity(
        authority,
        read,
        &identity.into(),
        policy.statement_timeout_ms,
    )?;
    let xid = bound.bind(connection.connection()).await?;
    Ok(Arc::new(Transaction::new(
        connection,
        Arc::clone(postgres.destroyed_connections()),
        xid,
        causation,
    )))
}

pub(crate) async fn claim(
    postgres: &WamnPostgres,
    target: DatabaseTarget<'_>,
    operation: &CanonicalName,
    key: &str,
    request: Vec<u8>,
) -> Result<Claim, SqlError> {
    WriteLog { postgres, target }
        .claim(operation, key, request)
        .await
}

pub(crate) async fn complete(
    postgres: &WamnPostgres,
    target: DatabaseTarget<'_>,
    operation: &CanonicalName,
    key: &str,
    result: String,
) -> Result<(), SqlError> {
    WriteLog { postgres, target }
        .complete(operation, key, result)
        .await
}

pub(crate) async fn release(
    postgres: &WamnPostgres,
    target: DatabaseTarget<'_>,
    operation: &CanonicalName,
    key: &str,
) -> Result<(), SqlError> {
    WriteLog { postgres, target }
        .release_claim(operation, key)
        .await
}

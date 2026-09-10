//! Compile refusals for committing a pending generated claim.

mod generated {
    include!(env!("WAMN_SPLIT_PROBE_ACCESSORS"));
}

use wamn_postgres_statements::{Connection, StatementError};

/// Attempt to commit without finalization through the pending claim.
#[cfg(feature = "early-commit")]
pub async fn early_commit(connection: &mut Connection) -> Result<(), StatementError> {
    let pending = generated::begin_claim(connection.begin().await?);
    pending.commit().await
}

/// Attempt to commit the transaction after the claim scope consumes it.
#[cfg(feature = "moved-transaction")]
pub async fn moved_transaction(connection: &mut Connection) -> Result<(), StatementError> {
    let transaction = connection.begin().await?;
    let _pending = generated::begin_claim(transaction);
    transaction.commit().await
}

/// Attempt to extract the pending claim's private transaction.
#[cfg(feature = "private-transaction")]
pub async fn private_transaction(connection: &mut Connection) -> Result<(), StatementError> {
    let pending = generated::begin_claim(connection.begin().await?);
    pending.transaction.commit().await
}

//! The command `sample.record` over the generated statements.
//!
//! The generated codec of `sample.record` claims the key in the write log and
//! holds the transaction. The command inserts the sample, whose id its default
//! mints.

use wamn_postgres_statements::Transaction;

use crate::error::{self, AccessError};
use crate::scalar;
use crate::statements::wamn::sample_record as record_sql;

/// One forwarded sample, as the edge box sends it.
#[derive(Debug)]
pub struct RecordCommand {
    pub frame: String,
    pub captured_at: String,
}

/// Record one sample in the transaction its codec holds, and answer its id.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn record(
    transaction: &mut Transaction,
    command: &RecordCommand,
) -> Result<String, AccessError> {
    let frame = scalar::text("value.frame", &command.frame)?;
    let captured_at = scalar::timestamp("value.captured_at", &command.captured_at)?;
    record_sql::record_sample(transaction, frame.to_owned(), captured_at)
        .await
        .map(|row| row.id.0)
        .map_err(|e| error::from_statement(&e))
}

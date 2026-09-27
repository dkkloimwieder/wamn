//! `sample.get` and the command `sample.record` over the generated statements.
//!
//! The generated codec of `sample.record` claims the key in the write log and
//! holds the transaction. The command inserts the sample, whose id its default
//! mints.

use wamn_postgres_statements::{Connection, Transaction};

use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::sample as sql;
use crate::generated::wamn::sample_record as record_sql;
use crate::scalar;

pub use crate::generated::wamn::sample::SampleRow;

/// Load one sample by id.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn get(connection: &mut Connection, id: &str) -> Result<SampleRow, AccessError> {
    let id = scalar::uuid("id", id)?;
    sql::get(connection, id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(|| AccessError::missing(AccessErrorKind::NotFound, "id", &id.0))
}

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

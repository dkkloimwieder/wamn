//! `sample.get` and the command `sample.record` over the generated statements.
//!
//! ```text
//! canonicalize the body
//! → find a replay: same key ⇒ return the original sample id, unchanged
//! → claim the key, which pre-generates the sample id
//! → insert the sample under that id, which finalizes the claim
//! ```

use wamn_postgres_statements::Connection;

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
    pub idempotency_key: String,
    pub frame: String,
    pub captured_at: String,
}

/// Record one sample under the id its claim row mints.
///
/// A repeat of the same key answers the id of the first attempt, and the
/// same key with another frame or time refuses as `idempotency_conflict`.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn record(command: &RecordCommand) -> Result<String, AccessError> {
    let frame = scalar::text("value.frame", &command.frame)?;
    let captured_at = scalar::timestamp("value.captured_at", &command.captured_at)?;
    // The key and request_id are excluded, as the canonicalization declares.
    let canonical = wamn_execution_contract::canonical_json_bytes(&serde_json::json!({
        "frame": frame,
        "captured_at": captured_at.0,
    }));
    let key = command.idempotency_key.clone();

    let mut connection = Connection::new();
    let transaction = connection
        .begin()
        .await
        .map_err(|e| error::from_statement(&e))?;
    let mut claim = record_sql::begin_claim(transaction);
    if let Some(replay) = record_sql::find_sample(&mut claim, key.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
    {
        return replayed(replay, &canonical);
    }
    let Some(claimed) = record_sql::claim_sample(&mut claim, canonical.clone(), key.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
    else {
        // Another caller took the key between the replay read and this insert.
        // Its row is committed, so the replay read now answers.
        let replay = record_sql::find_sample(&mut claim, key)
            .await
            .map_err(|e| error::from_statement(&e))?
            .ok_or_else(|| AccessError::new(AccessErrorKind::Retry, serde_json::json!({})))?;
        return replayed(replay, &canonical);
    };
    let sample_id = claimed.sample_id.0.clone();
    let finalized =
        record_sql::record_sample(claim, claimed.sample_id, frame.to_owned(), captured_at)
            .await
            .map_err(|e| error::from_statement(&e))?;
    finalized
        .commit()
        .await
        .map_err(|e| error::from_statement(&e))?;
    Ok(sample_id)
}

fn replayed(replay: record_sql::FindSampleRow, canonical: &[u8]) -> Result<String, AccessError> {
    if replay.canonical_command != canonical {
        return Err(AccessError::field(
            AccessErrorKind::IdempotencyConflict,
            "value.idempotency_key",
        ));
    }
    Ok(replay.sample_id.0)
}

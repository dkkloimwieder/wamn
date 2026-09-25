//! `packaging.close` with atomic state and stored replay results.
use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::packaging_close as sql;
use crate::scalar;
use serde::{Deserialize, Serialize};
use wamn_postgres_statements::Connection;

#[derive(Debug, Deserialize)]
pub struct CloseCommand {
    pub idempotency_key: String,
    pub packaging_id: String,
    pub expected_row_version: i32,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloseResult {
    pub operation_id: String,
    pub packaging_id: String,
    pub r#type: String,
    pub code: String,
    pub location_id: String,
    pub lifecycle: String,
    pub row_version: i32,
}

/// Apply one command or return its complete original result.
///
/// # Errors
/// Returns the refusal declared by the operation contract.
pub async fn execute(command: &CloseCommand) -> Result<CloseResult, AccessError> {
    scalar::text("value.idempotency_key", Some(&command.idempotency_key))?;
    let packaging_id = scalar::uuid("value.packaging_id", &command.packaging_id)?;
    let expected_row_version = command.expected_row_version;
    let canonical = wamn_execution_contract::canonical_json_bytes(&serde_json::json!({
        "packaging_id": packaging_id.0,
        "expected_row_version": expected_row_version,
    }));
    let mut connection = Connection::new();
    let mut transaction = sql::begin_claim(
        connection
            .begin()
            .await
            .map_err(|e| error::from_statement(&e))?,
    );
    if let Some(replay) = sql::find_replay(&mut transaction, command.idempotency_key.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
    {
        if replay.canonical_command != canonical {
            return Err(AccessError::field(
                AccessErrorKind::IdempotencyConflict,
                "value.idempotency_key",
            ));
        }
        let result = replay.result.ok_or_else(retry)?;
        return serde_json::from_str(&result).map_err(|_| internal());
    }
    let claim = sql::claim_command(
        &mut transaction,
        command.idempotency_key.clone(),
        canonical.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    .ok_or_else(retry)?;
    let source = sql::lock_packaging(&mut transaction, packaging_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(|| missing("value.packaging_id", &packaging_id.0))?;
    if source.lifecycle != "open" {
        return Err(invalid("value.packaging_id"));
    }
    if source.row_version != expected_row_version {
        return Err(AccessError::conflict(
            expected_row_version,
            source.row_version,
        ));
    }
    if sql::open_inventory(&mut transaction, packaging_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .is_some()
    {
        return Err(invalid("value.packaging_id"));
    }
    let result_row = sql::apply(&mut transaction, packaging_id)
        .await
        .map_err(|e| error::from_statement(&e))?;
    let result = CloseResult {
        operation_id: claim.operation_id.0.clone(),
        packaging_id: result_row.id.0,
        r#type: result_row.r#type,
        code: result_row.code,
        location_id: result_row.location_id.0,
        lifecycle: result_row.lifecycle,
        row_version: result_row.row_version,
    };
    let stored = serde_json::to_string(&result).map_err(|_| internal())?;
    let finalized = sql::finalize_command(
        transaction,
        command.idempotency_key.clone(),
        canonical,
        claim.operation_id,
        stored,
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    if finalized.row.result.is_none() {
        return Err(retry());
    }
    finalized
        .commit()
        .await
        .map_err(|e| error::from_statement(&e))?;
    Ok(result)
}

fn retry() -> AccessError {
    AccessError::new(AccessErrorKind::Retry, serde_json::json!({}))
}
fn internal() -> AccessError {
    AccessError::new(AccessErrorKind::InternalError, serde_json::json!({}))
}
fn missing(field: &str, id: &str) -> AccessError {
    AccessError::missing(AccessErrorKind::NotFound, field, id)
}
fn invalid(field: &str) -> AccessError {
    AccessError::field(AccessErrorKind::InvalidInput, field)
}

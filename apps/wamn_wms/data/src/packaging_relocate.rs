//! `packaging.relocate` with atomic state and stored replay results.
use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::packaging_relocate as sql;
use crate::scalar;
use serde::{Deserialize, Serialize};
use wamn_postgres_statements::Connection;

#[derive(Debug, Deserialize)]
pub struct RelocateCommand {
    pub idempotency_key: String,
    pub packaging_id: String,
    pub expected_row_version: i32,
    pub to_location_id: String,
    pub occurred_at: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelocateResult {
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
pub async fn execute(command: &RelocateCommand) -> Result<RelocateResult, AccessError> {
    scalar::text("value.idempotency_key", Some(&command.idempotency_key))?;
    let packaging_id = scalar::uuid("value.packaging_id", &command.packaging_id)?;
    let expected_row_version = command.expected_row_version;
    let to_location_id = scalar::uuid("value.to_location_id", &command.to_location_id)?;
    let occurred_at = scalar::timestamp("value.occurred_at", &command.occurred_at)?;
    let canonical = wamn_execution_contract::canonical_json_bytes(&serde_json::json!({
        "packaging_id": packaging_id.0,
        "expected_row_version": expected_row_version,
        "to_location_id": to_location_id.0,
        "occurred_at": occurred_at.0,
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
    // Keep the inventory-first lock order used by every inventory command.
    let inventory = sql::lock_inventory(&mut transaction, packaging_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?;
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
    // The first SELECT cannot see a member committed after its snapshot. Re-read
    // membership under the packaging lock without reversing the lock order.
    let members = sql::open_inventory(&mut transaction, packaging_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?;
    if !inventory
        .iter()
        .map(|row| &row.id)
        .eq(members.iter().map(|row| &row.id))
    {
        return Err(retry());
    }
    if source.location_id == to_location_id {
        return Err(invalid("value.to_location_id"));
    }
    if inventory
        .iter()
        .any(|row| row.location_id != source.location_id)
    {
        return Err(invalid("value.packaging_id"));
    }
    sql::validate_location(&mut transaction, to_location_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(|| missing("value.to_location_id", &to_location_id.0))?;
    let result_row = sql::apply(&mut transaction, packaging_id, to_location_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?;
    for row in inventory {
        sql::apply_inventory(&mut transaction, row.id.clone(), to_location_id.clone())
            .await
            .map_err(|e| error::from_statement(&e))?;
        sql::insert_transaction(
            &mut transaction,
            claim.operation_id.clone(),
            row.id.clone(),
            row.id.clone(),
            row.id.clone(),
            Some(row.product_id),
            Some(row.packaging_id),
            Some(row.location_id),
            row.quantity,
            Some(row.disposition),
            Some(row.lifecycle),
            occurred_at.clone(),
            None,
        )
        .await
        .map_err(|e| error::from_statement(&e))?;
    }
    let result = RelocateResult {
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

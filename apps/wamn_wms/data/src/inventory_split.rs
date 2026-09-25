//! `inventory.split` with atomic state, immutable history, and stored replay results.
use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::inventory_split as sql;
use crate::scalar;
use serde::{Deserialize, Serialize};
use wamn_postgres_statements::{Connection, Numeric};

#[derive(Debug, Deserialize)]
pub struct SplitCommand {
    pub idempotency_key: String,
    pub from_inventory_id: String,
    pub quantity: String,
    pub to_packaging_id: String,
    pub to_location_id: String,
    pub expected_row_version: i32,
    pub occurred_at: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SplitResult {
    pub operation_id: String,
    pub inventory_id: String,
    pub product_id: String,
    pub packaging_id: String,
    pub location_id: String,
    pub quantity: String,
    pub disposition: String,
    pub lifecycle: String,
    pub row_version: i32,
    pub new_inventory_id: String,
}

/// Apply one command or return its complete original result.
///
/// # Errors
/// Returns the refusal declared by the operation contract.
pub async fn execute(command: &SplitCommand) -> Result<SplitResult, AccessError> {
    scalar::text("value.idempotency_key", Some(&command.idempotency_key))?;
    let from_inventory_id = scalar::uuid("value.from_inventory_id", &command.from_inventory_id)?;
    let quantity = scalar::numeric("value.quantity", &command.quantity)?;
    let to_packaging_id = scalar::uuid("value.to_packaging_id", &command.to_packaging_id)?;
    let to_location_id = scalar::uuid("value.to_location_id", &command.to_location_id)?;
    let expected_row_version = command.expected_row_version;
    let occurred_at = scalar::timestamp("value.occurred_at", &command.occurred_at)?;
    let canonical = wamn_execution_contract::canonical_json_bytes(&serde_json::json!({
        "from_inventory_id": from_inventory_id.0,
        "quantity": quantity.0,
        "to_packaging_id": to_packaging_id.0,
        "to_location_id": to_location_id.0,
        "expected_row_version": expected_row_version,
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
    let source = sql::lock_inventory(&mut transaction, from_inventory_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(|| missing("value.from_inventory_id", &from_inventory_id.0))?;
    require_open(&source, expected_row_version)?;
    let packaging = sql::lock_packaging(
        &mut transaction,
        source.packaging_id.clone(),
        to_packaging_id.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    let source_packaging = packaging
        .iter()
        .find(|row| row.id == source.packaging_id)
        .ok_or_else(|| missing("value.inventory_id", &source.id.0))?;
    if source_packaging.lifecycle != "open" || source_packaging.location_id != source.location_id {
        return Err(invalid("value.inventory_id"));
    }
    let destination = packaging
        .iter()
        .find(|row| row.id == to_packaging_id)
        .ok_or_else(|| missing("value.to_packaging_id", &to_packaging_id.0))?;
    if destination.lifecycle != "open" {
        return Err(invalid("value.to_packaging_id"));
    }
    if destination.location_id != to_location_id {
        return Err(invalid("value.to_location_id"));
    }
    let result_row = sql::apply(
        &mut transaction,
        from_inventory_id.clone(),
        quantity.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    .ok_or_else(|| AccessError::field(AccessErrorKind::InsufficientQuantity, "value.quantity"))?;
    sql::create_inventory(
        &mut transaction,
        claim.new_inventory_id.clone(),
        source.product_id.clone(),
        to_packaging_id,
        to_location_id,
        quantity,
        source.disposition.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    sql::insert_transaction(
        &mut transaction,
        claim.operation_id.clone(),
        from_inventory_id.clone(),
        from_inventory_id.clone(),
        from_inventory_id.clone(),
        Some(source.product_id.clone()),
        Some(source.packaging_id.clone()),
        Some(source.location_id.clone()),
        source.quantity.clone(),
        Some(source.disposition.clone()),
        Some(source.lifecycle.clone()),
        occurred_at.clone(),
        None,
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    sql::insert_transaction(
        &mut transaction,
        claim.operation_id.clone(),
        claim.new_inventory_id.clone(),
        from_inventory_id.clone(),
        claim.new_inventory_id.clone(),
        None,
        None,
        None,
        Numeric("0".into()),
        None,
        None,
        occurred_at.clone(),
        None,
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    let result = SplitResult {
        operation_id: claim.operation_id.0.clone(),
        inventory_id: result_row.id.0,
        product_id: result_row.product_id.0,
        packaging_id: result_row.packaging_id.0,
        location_id: result_row.location_id.0,
        quantity: result_row.quantity.0,
        disposition: result_row.disposition,
        lifecycle: result_row.lifecycle,
        row_version: result_row.row_version,
        new_inventory_id: claim.new_inventory_id.0,
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
fn require_open(row: &sql::LockInventoryRow, expected: i32) -> Result<(), AccessError> {
    if row.lifecycle != "open" {
        return Err(invalid("value.inventory_id"));
    }
    if row.row_version != expected {
        return Err(AccessError::conflict(expected, row.row_version));
    }
    Ok(())
}

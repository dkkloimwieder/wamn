//! `inventory.split` with atomic state, immutable history, and stored replay results.
use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::inventory_split as sql;
use crate::scalar;
use serde::{Deserialize, Serialize};
use wamn_postgres_statements::{Connection, Numeric, Uuid};

mod decision;

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
    let inventory = decision::Inventory {
        id: &source.id.0,
        product_id: &source.product_id.0,
        packaging_id: &source.packaging_id.0,
        location_id: &source.location_id.0,
        quantity: source.quantity.0.clone(),
        disposition: &source.disposition,
        lifecycle: &source.lifecycle,
    };
    decision::require_open(&inventory)
        .map_err(|refusal| business_refusal(refusal, &source.id.0, &to_packaging_id.0))?;
    if source.row_version != expected_row_version {
        return Err(AccessError::conflict(
            expected_row_version,
            source.row_version,
        ));
    }
    let packaging = sql::lock_packaging(
        &mut transaction,
        source.packaging_id.clone(),
        to_packaging_id.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    let source_packaging = packaging.iter().find(|row| row.id == source.packaging_id);
    let destination = packaging.iter().find(|row| row.id == to_packaging_id);
    let transition = decision::decide(
        decision::State {
            source: &inventory,
            source_packaging: source_packaging.map(business_packaging),
            destination: destination.map(business_packaging),
        },
        decision::Command {
            new_inventory_id: &claim.new_inventory_id.0,
            quantity: &quantity.0,
            to_packaging_id: &to_packaging_id.0,
            to_location_id: &to_location_id.0,
        },
    )
    .map_err(|refusal| business_refusal(refusal, &source.id.0, &to_packaging_id.0))?;
    let [remaining, created] = &transition.inventory;
    let result_row = sql::apply(
        &mut transaction,
        Uuid(remaining.id.to_owned()),
        Numeric(remaining.quantity.clone()),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    .ok_or_else(internal)?;
    sql::create_inventory(
        &mut transaction,
        Uuid(created.id.to_owned()),
        Uuid(created.product_id.to_owned()),
        Uuid(created.packaging_id.to_owned()),
        Uuid(created.location_id.to_owned()),
        Numeric(created.quantity.clone()),
        created.disposition.to_owned(),
        created.lifecycle.to_owned(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    for row in &transition.transactions {
        let from = row.from_inventory.as_ref();
        let to = &row.to_inventory;
        sql::insert_transaction(
            &mut transaction,
            claim.operation_id.clone(),
            Uuid(row.inventory_id.to_owned()),
            Uuid(row.from_inventory_id.to_owned()),
            Uuid(row.to_inventory_id.to_owned()),
            from.map(|item| Uuid(item.product_id.to_owned())),
            from.map(|item| Uuid(item.packaging_id.to_owned())),
            from.map(|item| Uuid(item.location_id.to_owned())),
            Numeric(from.map_or_else(|| "0".to_owned(), |item| item.quantity.clone())),
            from.map(|item| item.disposition.to_owned()),
            from.map(|item| item.lifecycle.to_owned()),
            occurred_at.clone(),
            None,
            Uuid(to.product_id.to_owned()),
            Uuid(to.packaging_id.to_owned()),
            Uuid(to.location_id.to_owned()),
            Numeric(to.quantity.clone()),
            to.disposition.to_owned(),
            to.lifecycle.to_owned(),
        )
        .await
        .map_err(|e| error::from_statement(&e))?;
    }
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
fn business_packaging(row: &sql::LockPackagingRow) -> decision::Packaging<'_> {
    decision::Packaging {
        id: &row.id.0,
        location_id: &row.location_id.0,
        lifecycle: &row.lifecycle,
    }
}

fn business_refusal(
    refusal: decision::Refusal,
    inventory_id: &str,
    packaging_id: &str,
) -> AccessError {
    match refusal.r#type {
        decision::RefusalType::ClosedInventory | decision::RefusalType::InvalidSourcePackaging => {
            invalid("value.inventory_id")
        }
        decision::RefusalType::MissingSourcePackaging => {
            missing("value.inventory_id", inventory_id)
        }
        decision::RefusalType::MissingDestination => missing("value.to_packaging_id", packaging_id),
        decision::RefusalType::ClosedDestination => invalid("value.to_packaging_id"),
        decision::RefusalType::DestinationLocationMismatch => invalid("value.to_location_id"),
        decision::RefusalType::InsufficientQuantity => {
            AccessError::field(AccessErrorKind::InsufficientQuantity, "value.quantity")
        }
    }
}

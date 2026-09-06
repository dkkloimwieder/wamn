//! `inventory.split` -- part of one quantity row moved onto a NEW pallet.
//!
//! ```text
//! canonicalize the body
//! → find a replay: same key ⇒ return the ORIGINAL result, unchanged
//! → claim the key, which pre-generates the movement id AND the new pallet id
//! → lock the source pallet     (the serialization point)
//! → compare expected_row_version to observed
//! → validate the destination
//! → read the quantity row, then take from it (it must keep stock)
//! → create the new pallet, place the quantity on it, write the movement
//! → bump the source's revision
//! → finalize the claim with the result
//! ```
//!
//! The new pallet's id comes from the claim, exactly as the movement id does:
//! a split that minted it during the work would mint a SECOND pallet on
//! replay and leave real stock on a ghost (`claim_command.sql`). The new
//! pallet inherits the source's status -- a split of a held pallet does not
//! release the hold. The source must keep stock: moving everything is a move.

use serde::Deserialize;
use wamn_postgres_statements::{Connection, Numeric, TimestampTz, Transaction, Uuid};

use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::inventory_split as sql;
use crate::scalar;

pub(crate) const REFUSALS: &[AccessErrorKind] = &[
    AccessErrorKind::InvalidInput,
    AccessErrorKind::PalletNotFound,
    AccessErrorKind::LocationNotFound,
    AccessErrorKind::QuantityNotFound,
    AccessErrorKind::InsufficientQuantity,
    AccessErrorKind::ConcurrencyConflict,
    AccessErrorKind::IdempotencyConflict,
    AccessErrorKind::Retry,
    AccessErrorKind::Timeout,
    AccessErrorKind::PermissionDenied,
    AccessErrorKind::InternalError,
];

/// One envelope item's command body.
#[derive(Debug, Deserialize)]
pub(crate) struct SplitCommand {
    idempotency_key: String,
    source_pallet_id: String,
    product_id: String,
    status: String,
    quantity: String,
    new_pallet_code: String,
    to_location_id: String,
    expected_row_version: i64,
    occurred_at: String,
}

/// What one accepted split answers with.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SplitResult {
    movement_id: String,
    source_pallet_id: String,
    new_pallet_id: String,
    source_status: String,
    row_version: i64,
}

impl SplitResult {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "movement_id": self.movement_id,
            "source_pallet_id": self.source_pallet_id,
            "new_pallet_id": self.new_pallet_id,
            "source_status": self.source_status,
            "row_version": self.row_version,
        })
    }
}

#[derive(Debug)]
struct Parsed {
    source_pallet_id: Uuid,
    product_id: Uuid,
    status: String,
    quantity: Numeric,
    to_location_id: Uuid,
    occurred_at: TimestampTz,
}

fn parse(command: &SplitCommand) -> Result<Parsed, AccessError> {
    if command.new_pallet_code.is_empty() {
        return Err(AccessError::field(
            AccessErrorKind::InvalidInput,
            "value.new_pallet_code",
        ));
    }
    Ok(Parsed {
        source_pallet_id: scalar::uuid("value.source_pallet_id", &command.source_pallet_id)?,
        product_id: scalar::uuid("value.product_id", &command.product_id)?,
        status: scalar::quantity_status("value.status", &command.status)?,
        quantity: scalar::numeric("value.quantity", &command.quantity)?,
        to_location_id: scalar::uuid("value.to_location_id", &command.to_location_id)?,
        occurred_at: scalar::timestamp("value.occurred_at", &command.occurred_at)?,
    })
}

fn canonical_command(command: &SplitCommand, parsed: &Parsed) -> Vec<u8> {
    wamn_execution_contract::canonical_json_bytes(&serde_json::json!({
        "source_pallet_id": parsed.source_pallet_id.0,
        "product_id": parsed.product_id.0,
        "status": parsed.status,
        "quantity": parsed.quantity.0,
        "new_pallet_code": command.new_pallet_code,
        "to_location_id": parsed.to_location_id.0,
        "expected_row_version": command.expected_row_version,
        "occurred_at": parsed.occurred_at.0,
    }))
}

/// Run one command item in exactly one transaction.
///
/// # Errors
///
/// [`AccessError`] carrying the literal and detail the operation contract
/// declares for that refusal.
pub(crate) async fn execute(command: &SplitCommand) -> Result<SplitResult, AccessError> {
    let parsed = parse(command)?;
    let canonical = canonical_command(command, &parsed);

    let mut connection = Connection::new();
    let mut transaction = connection
        .begin()
        .await
        .map_err(|e| error::from_statement(&e))?;
    let result = run(&mut transaction, command, &canonical, &parsed).await;
    match result {
        Ok(value) => {
            transaction
                .commit()
                .await
                .map_err(|e| error::from_statement(&e))?;
            Ok(value)
        }
        Err(refusal) => {
            let _ = transaction.rollback().await;
            Err(refusal)
        }
    }
}

fn retry() -> AccessError {
    AccessError::new(AccessErrorKind::Retry, serde_json::json!({}))
}

fn internal() -> AccessError {
    AccessError::new(AccessErrorKind::InternalError, serde_json::json!({}))
}

async fn run(
    transaction: &mut Transaction,
    command: &SplitCommand,
    canonical: &[u8],
    parsed: &Parsed,
) -> Result<SplitResult, AccessError> {
    let key = command.idempotency_key.clone();
    if let Some(replay) = sql::find_replay(transaction, key.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
    {
        if replay.canonical_command != canonical {
            return Err(AccessError::field(
                AccessErrorKind::IdempotencyConflict,
                "value.idempotency_key",
            ));
        }
        let Some(row_version) = replay.row_version else {
            return Err(retry());
        };
        // The source's status was never this command's to change, so the
        // live row's is the original's.
        let source = sql::lock_pallet(transaction, replay.source_pallet_id.clone())
            .await
            .map_err(|e| error::from_statement(&e))?
            .ok_or_else(internal)?;
        return Ok(SplitResult {
            movement_id: replay.movement_id.0,
            source_pallet_id: replay.source_pallet_id.0,
            new_pallet_id: replay.new_pallet_id.0,
            source_status: source.status,
            row_version,
        });
    }

    let claim = sql::claim_command(
        transaction,
        key.clone(),
        canonical.to_vec(),
        parsed.source_pallet_id.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    .ok_or_else(retry)?;

    // THE SERIALIZATION POINT.
    let not_found = || {
        AccessError::missing(
            AccessErrorKind::PalletNotFound,
            "value.source_pallet_id",
            &parsed.source_pallet_id.0,
        )
    };
    let locked = sql::lock_pallet(transaction, parsed.source_pallet_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(not_found)?;
    if locked.status == scalar::CONSUMED {
        return Err(not_found());
    }
    if locked.row_version != command.expected_row_version {
        return Err(AccessError::conflict(
            command.expected_row_version,
            locked.row_version,
        ));
    }

    sql::validate_location(transaction, parsed.to_location_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(|| {
            AccessError::missing(
                AccessErrorKind::LocationNotFound,
                "value.to_location_id",
                &parsed.to_location_id.0,
            )
        })?;

    // Read before taking, so the refusal can say which of two things is
    // wrong: no such row, or a row that cannot spare what was asked.
    let held = sql::select_quantity(
        transaction,
        parsed.source_pallet_id.clone(),
        parsed.product_id.clone(),
        parsed.status.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    .ok_or_else(|| {
        AccessError::missing(
            AccessErrorKind::QuantityNotFound,
            "value.product_id",
            &parsed.product_id.0,
        )
    })?;
    sql::take_from_source(
        transaction,
        parsed.source_pallet_id.clone(),
        parsed.product_id.clone(),
        parsed.status.clone(),
        parsed.quantity.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    .ok_or_else(|| AccessError::insufficient("value.quantity", &held.quantity.0))?;

    sql::create_pallet(
        transaction,
        claim.new_pallet_id.clone(),
        command.new_pallet_code.clone(),
        parsed.to_location_id.clone(),
        locked.status.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    sql::place_quantity(
        transaction,
        claim.new_pallet_id.clone(),
        parsed.product_id.clone(),
        parsed.status.clone(),
        parsed.quantity.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    sql::insert_movement(
        transaction,
        key.clone(),
        parsed.source_pallet_id.clone(),
        parsed.product_id.clone(),
        parsed.quantity.clone(),
        parsed.occurred_at.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;

    let touched = sql::touch_source(transaction, parsed.source_pallet_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?;

    let finalized = sql::finalize_command(
        transaction,
        key,
        canonical.to_vec(),
        claim.movement_id.clone(),
        touched.row_version,
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    if finalized.row_version.is_none() {
        return Err(retry());
    }

    Ok(SplitResult {
        movement_id: claim.movement_id.0,
        source_pallet_id: parsed.source_pallet_id.0.clone(),
        new_pallet_id: claim.new_pallet_id.0,
        source_status: touched.status,
        row_version: touched.row_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command() -> SplitCommand {
        SplitCommand {
            idempotency_key: "k".to_owned(),
            source_pallet_id: "00000000-0000-0000-0000-000000000301".to_owned(),
            product_id: "00000000-0000-0000-0000-000000000101".to_owned(),
            status: "available".to_owned(),
            quantity: "4".to_owned(),
            new_pallet_code: "PAL-302".to_owned(),
            to_location_id: "00000000-0000-0000-0000-000000000202".to_owned(),
            expected_row_version: 1,
            occurred_at: "2026-09-05T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn the_canonical_command_excludes_the_key_and_names_every_other_field() {
        let command = command();
        let bytes = canonical_command(&command, &parse(&command).unwrap());
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("idempotency_key"));
        for field in [
            "source_pallet_id",
            "product_id",
            "status",
            "quantity",
            "new_pallet_code",
            "to_location_id",
            "expected_row_version",
            "occurred_at",
        ] {
            assert!(text.contains(field), "{field} is part of the identity");
        }
    }

    #[test]
    fn a_blank_pallet_code_and_a_zero_quantity_refuse_before_any_statement() {
        let mut blank = command();
        blank.new_pallet_code.clear();
        assert_eq!(
            parse(&blank).unwrap_err().detail()["field"],
            "value.new_pallet_code"
        );
        let mut zero = command();
        zero.quantity = "0.0".to_owned();
        assert_eq!(
            parse(&zero).unwrap_err().detail()["field"],
            "value.quantity"
        );
    }
}

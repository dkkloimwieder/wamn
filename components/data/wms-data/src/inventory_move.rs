//! `inventory.move` — the contended command.
//!
//! One pallet moved between locations, transactionally, multi-row, with
//! optimistic concurrency at the row that actually contends. The generator
//! wrote every statement and its decoding; what is authored here is the ORDER
//! those statements run in and what each refusal means.
//!
//! # The shape of one item
//!
//! ```text
//! canonicalize the body
//! → find a replay: same key ⇒ return the ORIGINAL result, unchanged
//! → claim the key, which pre-generates the movement id
//! → lock the pallet          (the serialization point)
//! → compare expected_row_version to observed
//! → validate the destination
//! → write one movement per quantity row
//! → move the pallet and bump its revision
//! → finalize the claim with the result
//! ```
//!
//! The lock is on `pallet` and not on `pallet_quantity` deliberately: two
//! concurrent moves of one pallet must not both succeed by touching different
//! quantity rows, and the pallet is what makes them serialize.

use serde::Deserialize;
use wamn_postgres_statements::{Connection, TimestampTz, Transaction, Uuid};

use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::inventory_move as sql;

/// One envelope item's command body.
#[derive(Debug, Deserialize)]
pub(crate) struct MoveCommand {
    pub(crate) idempotency_key: String,
    pub(crate) pallet_id: String,
    pub(crate) to_location_id: String,
    pub(crate) expected_row_version: i64,
    pub(crate) occurred_at: String,
}

/// What one accepted move answers with.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MoveResult {
    pub(crate) movement_id: String,
    pub(crate) pallet_id: String,
    pub(crate) location_id: String,
    pub(crate) pallet_status: String,
    pub(crate) row_version: i64,
}

impl MoveResult {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "movement_id": self.movement_id,
            "pallet_id": self.pallet_id,
            "location_id": self.location_id,
            "pallet_status": self.pallet_status,
            "row_version": self.row_version,
        })
    }
}

/// Parse and RE-SPELL, not merely validate. The canonicalization contract
/// fixes uuids as lowercase-hyphenated, so a caller sending uppercase must
/// reach the database — and the command bytes — in one spelling, or two
/// deliveries of the same move would canonicalize differently and the
/// idempotency key would stop working.
fn uuid(field: &str, value: &str) -> Result<Uuid, AccessError> {
    value
        .parse::<uuid::Uuid>()
        .map(|parsed| Uuid(parsed.hyphenated().to_string()))
        .map_err(|_| AccessError::field(AccessErrorKind::InvalidInput, field))
}

/// Likewise for timestamps: UTC, RFC 3339, six fractional digits, whatever
/// offset the caller wrote it in.
fn timestamp(field: &str, value: &str) -> Result<TimestampTz, AccessError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|parsed| TimestampTz(parsed.to_utc().format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()))
        .map_err(|_| AccessError::field(AccessErrorKind::InvalidInput, field))
}

/// Run one command item in exactly one transaction.
///
/// # Errors
///
/// [`AccessError`] carrying the literal and detail the operation contract
/// declares for that refusal.
pub(crate) async fn execute(command: &MoveCommand) -> Result<MoveResult, AccessError> {
    let pallet_id = uuid("value.pallet_id", &command.pallet_id)?;
    let to_location_id = uuid("value.to_location_id", &command.to_location_id)?;
    let occurred_at = timestamp("value.occurred_at", &command.occurred_at)?;
    let canonical = canonical_command(command);

    let mut connection = Connection::new();
    let mut transaction = connection
        .begin()
        .await
        .map_err(|e| error::from_statement(&e))?;
    let result = run(
        &mut transaction,
        command,
        &canonical,
        pallet_id.clone(),
        to_location_id,
        occurred_at,
    )
    .await;
    match result {
        Ok(value) => {
            transaction
                .commit()
                .await
                .map_err(|e| error::from_statement(&e))?;
            Ok(value)
        }
        // Dropping the transaction rolls it back; the explicit rollback makes
        // the refusal path say so rather than relying on a Drop nobody reads.
        Err(refusal) => {
            let _ = transaction.rollback().await;
            Err(refusal)
        }
    }
}

/// The bytes the idempotency key keys.
///
/// The key itself and `request_id` are EXCLUDED: two deliveries of one
/// operator action differ in neither the pallet moved nor its destination,
/// only in the envelope that carried them. Canonical JSON gives sorted keys,
/// so the same command produces the same bytes whatever order it arrived in.
fn canonical_command(command: &MoveCommand) -> Vec<u8> {
    wamn_execution_contract::canonical_json_bytes(&serde_json::json!({
        "pallet_id": command.pallet_id,
        "to_location_id": command.to_location_id,
        "expected_row_version": command.expected_row_version,
        "occurred_at": command.occurred_at,
    }))
}

async fn run(
    transaction: &mut Transaction,
    command: &MoveCommand,
    canonical: &[u8],
    pallet_id: Uuid,
    to_location_id: Uuid,
    occurred_at: TimestampTz,
) -> Result<MoveResult, AccessError> {
    // A REPLAY RETURNS THE ORIGINAL RESULT, unchanged. Not a fresh execution
    // that happens to agree — the claim row holds what the first attempt
    // decided, including the movement id a downstream label key depends on.
    if let Some(replay) = sql::find_replay(transaction, command.idempotency_key.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
    {
        if replay.canonical_command != canonical {
            // Same key, different command. That is two moves wearing one
            // identity, and answering either would be wrong.
            return Err(AccessError::field(
                AccessErrorKind::IdempotencyConflict,
                "value.idempotency_key",
            ));
        }
        let (Some(pallet_status), Some(row_version)) = (replay.pallet_status, replay.row_version)
        else {
            // Claimed but never finalized: the first attempt died between the
            // claim and the commit. Retryable, and the retry will re-run the
            // work under the same claim.
            return Err(AccessError::new(
                AccessErrorKind::Retry,
                serde_json::json!({}),
            ));
        };
        return Ok(MoveResult {
            movement_id: replay.movement_id.0.clone(),
            pallet_id: replay.pallet_id.0.clone(),
            location_id: command.to_location_id.clone(),
            pallet_status,
            row_version,
        });
    }

    let claim = sql::claim_command(
        transaction,
        command.idempotency_key.clone(),
        canonical.to_vec(),
        pallet_id.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    .ok_or_else(|| AccessError::new(AccessErrorKind::Retry, serde_json::json!({})))?;

    // THE SERIALIZATION POINT.
    let locked = sql::lock_pallet(transaction, pallet_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(|| {
            AccessError::missing(
                AccessErrorKind::PalletNotFound,
                "value.pallet_id",
                &command.pallet_id,
            )
        })?;

    if locked.row_version != command.expected_row_version {
        return Err(AccessError::conflict(
            command.expected_row_version,
            locked.row_version,
        ));
    }

    sql::validate_location(transaction, to_location_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(|| {
            AccessError::missing(
                AccessErrorKind::LocationNotFound,
                "value.to_location_id",
                &command.to_location_id,
            )
        })?;

    // ONE MOVEMENT PER QUANTITY ROW. The history says WHAT moved, not merely
    // that something did — which is the multi-row half of this command.
    let quantities = sql::select_pallet_quantity(transaction, pallet_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?;
    for quantity in &quantities {
        sql::insert_movement(
            transaction,
            command.idempotency_key.clone(),
            pallet_id.clone(),
            quantity.product_id.clone(),
            locked.location_id.clone(),
            to_location_id.clone(),
            quantity.quantity.clone(),
            occurred_at.clone(),
        )
        .await
        .map_err(|e| error::from_statement(&e))?;
    }

    let moved = sql::move_pallet(transaction, pallet_id, to_location_id)
        .await
        .map_err(|e| error::from_statement(&e))?;

    let finalized = sql::finalize_command(
        transaction,
        command.idempotency_key.clone(),
        canonical.to_vec(),
        claim.movement_id.clone(),
        moved.status.clone(),
        moved.row_version,
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    if finalized.row_version.is_none() {
        // The guarded finalize matched nothing, so this claim was already
        // finalized by a concurrent attempt. Refusing beats reporting a result
        // this transaction did not write.
        return Err(AccessError::new(
            AccessErrorKind::Retry,
            serde_json::json!({}),
        ));
    }

    Ok(MoveResult {
        movement_id: claim.movement_id.0.clone(),
        pallet_id: command.pallet_id.clone(),
        location_id: moved.location_id.0.clone(),
        pallet_status: moved.status,
        row_version: moved.row_version,
    })
}

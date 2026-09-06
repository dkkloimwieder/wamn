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
use crate::scalar;

pub(crate) const REFUSALS: &[AccessErrorKind] = &[
    AccessErrorKind::InvalidInput,
    AccessErrorKind::PalletNotFound,
    AccessErrorKind::LocationNotFound,
    AccessErrorKind::ConcurrencyConflict,
    AccessErrorKind::IdempotencyConflict,
    AccessErrorKind::Retry,
    AccessErrorKind::Timeout,
    AccessErrorKind::PermissionDenied,
    AccessErrorKind::InternalError,
];

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

/// The command's scalars in their one wire spelling.
#[derive(Debug)]
struct Parsed {
    pallet_id: Uuid,
    to_location_id: Uuid,
    occurred_at: TimestampTz,
}

fn parse(command: &MoveCommand) -> Result<Parsed, AccessError> {
    Ok(Parsed {
        pallet_id: scalar::uuid("value.pallet_id", &command.pallet_id)?,
        to_location_id: scalar::uuid("value.to_location_id", &command.to_location_id)?,
        occurred_at: scalar::timestamp("value.occurred_at", &command.occurred_at)?,
    })
}

/// Run one command item in exactly one transaction.
///
/// # Errors
///
/// [`AccessError`] carrying the literal and detail the operation contract
/// declares for that refusal.
pub(crate) async fn execute(command: &MoveCommand) -> Result<MoveResult, AccessError> {
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
        // Dropping the transaction rolls it back; the explicit rollback makes
        // the refusal path say so rather than relying on a Drop nobody reads.
        Err(refusal) => {
            let _ = transaction.rollback().await;
            Err(refusal)
        }
    }
}

/// The bytes the idempotency key keys: the RE-SPELLED command, so two
/// deliveries of one move canonicalize alike whatever case or offset each was
/// written in.
///
/// The key itself and `request_id` are EXCLUDED: two deliveries of one
/// operator action differ in neither the pallet moved nor its destination,
/// only in the envelope that carried them. Canonical JSON gives sorted keys,
/// so the same command produces the same bytes whatever order it arrived in.
fn canonical_command(command: &MoveCommand, parsed: &Parsed) -> Vec<u8> {
    wamn_execution_contract::canonical_json_bytes(&serde_json::json!({
        "pallet_id": parsed.pallet_id.0,
        "to_location_id": parsed.to_location_id.0,
        "expected_row_version": command.expected_row_version,
        "occurred_at": parsed.occurred_at.0,
    }))
}

async fn run(
    transaction: &mut Transaction,
    command: &MoveCommand,
    canonical: &[u8],
    parsed: &Parsed,
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
        parsed.pallet_id.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    .ok_or_else(|| AccessError::new(AccessErrorKind::Retry, serde_json::json!({})))?;

    // THE SERIALIZATION POINT.
    let locked = sql::lock_pallet(transaction, parsed.pallet_id.clone())
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

    sql::validate_location(transaction, parsed.to_location_id.clone())
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
    let quantities = sql::select_pallet_quantity(transaction, parsed.pallet_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?;
    for quantity in &quantities {
        sql::insert_movement(
            transaction,
            command.idempotency_key.clone(),
            parsed.pallet_id.clone(),
            quantity.product_id.clone(),
            locked.location_id.clone(),
            parsed.to_location_id.clone(),
            quantity.quantity.clone(),
            parsed.occurred_at.clone(),
        )
        .await
        .map_err(|e| error::from_statement(&e))?;
    }

    let moved = sql::move_pallet(
        transaction,
        parsed.pallet_id.clone(),
        parsed.to_location_id.clone(),
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn command(pallet_id: &str, occurred_at: &str) -> MoveCommand {
        MoveCommand {
            idempotency_key: "k".to_owned(),
            pallet_id: pallet_id.to_owned(),
            to_location_id: "00000000-0000-0000-0000-000000000201".to_owned(),
            expected_row_version: 1,
            occurred_at: occurred_at.to_owned(),
        }
    }

    /// Two spellings of one move are ONE command under the key: the canonical
    /// bytes come from the respelled scalars, not the caller's. The uuid half
    /// is already refused at the input port, whose released pattern pins a
    /// lowercase-hyphenated uuid; the OFFSET half reaches this code, because
    /// the port only asks `occurred_at` for `format: date-time`.
    #[test]
    fn the_canonical_command_is_spelling_independent_and_excludes_the_key() {
        let upper = command(
            "00000000-0000-0000-0000-00000000030A",
            "2026-09-05T02:00:00+02:00",
        );
        let lower = command(
            "00000000-0000-0000-0000-00000000030a",
            "2026-09-05T00:00:00.000000Z",
        );
        let mut other_key = command(
            "00000000-0000-0000-0000-00000000030a",
            "2026-09-05T00:00:00Z",
        );
        other_key.idempotency_key = "different".to_owned();
        let bytes = |command: &MoveCommand| canonical_command(command, &parse(command).unwrap());
        assert_eq!(bytes(&upper), bytes(&lower));
        assert_eq!(bytes(&lower), bytes(&other_key));
        assert!(
            !String::from_utf8(bytes(&lower))
                .unwrap()
                .contains("idempotency_key")
        );
    }

    /// A move that differs in what it MOVES is a different command, so the
    /// bytes must still separate one from another.
    #[test]
    fn a_different_destination_is_a_different_command() {
        let here = command(
            "00000000-0000-0000-0000-00000000030a",
            "2026-09-05T00:00:00Z",
        );
        let mut there = command(
            "00000000-0000-0000-0000-00000000030a",
            "2026-09-05T00:00:00Z",
        );
        there.to_location_id = "00000000-0000-0000-0000-000000000202".to_owned();
        let bytes = |command: &MoveCommand| canonical_command(command, &parse(command).unwrap());
        assert_ne!(bytes(&here), bytes(&there));
    }

    #[test]
    fn an_unspellable_scalar_is_refused_before_any_statement() {
        let mut pallet = command("not-a-uuid", "2026-09-05T00:00:00Z");
        assert_eq!(
            parse(&pallet).unwrap_err().detail()["field"],
            "value.pallet_id"
        );
        pallet = command("00000000-0000-0000-0000-00000000030a", "yesterday");
        assert_eq!(
            parse(&pallet).unwrap_err().detail()["field"],
            "value.occurred_at"
        );
    }
}

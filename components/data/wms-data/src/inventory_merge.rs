//! `inventory.merge` -- one pallet absorbed into another.
//!
//! ```text
//! canonicalize the body
//! → find a replay: same key ⇒ return the ORIGINAL result, unchanged
//! → claim the key, which pre-generates the movement id
//! → lock BOTH pallets, in id order   (the serialization point)
//! → compare expected_row_version to the target's
//! → for each source quantity row: add it to the target's matching row,
//!   or place a new one, and write a movement
//! → consume the source (a tombstone: the platform admits no DELETE)
//! → bump the target's revision
//! → finalize the claim with the result
//! ```
//!
//! The revision the caller names is the TARGET's: that is the pallet the
//! command answers with and the one whose stock changes. The source only has
//! to be live, and once consumed it can never be merged again, so a stale
//! view of it has nothing to race. Both are locked in id order -- two merges
//! naming one pair in opposite orders cannot deadlock (`lock_both_pallets.sql`).
//! Movements are recorded against the source, the pallet the stock left.

use serde::Deserialize;
use wamn_postgres_statements::{Connection, TimestampTz, Transaction, Uuid};

use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::inventory_merge as sql;
use crate::scalar;

pub(crate) const REFUSALS: &[AccessErrorKind] = &[
    AccessErrorKind::InvalidInput,
    AccessErrorKind::PalletNotFound,
    AccessErrorKind::ConcurrencyConflict,
    AccessErrorKind::IdempotencyConflict,
    AccessErrorKind::Retry,
    AccessErrorKind::Timeout,
    AccessErrorKind::PermissionDenied,
    AccessErrorKind::InternalError,
];

/// One envelope item's command body.
#[derive(Debug, Deserialize)]
pub(crate) struct MergeCommand {
    idempotency_key: String,
    source_pallet_id: String,
    target_pallet_id: String,
    expected_row_version: i64,
    occurred_at: String,
}

/// What one accepted merge answers with.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MergeResult {
    movement_id: String,
    source_pallet_id: String,
    target_pallet_id: String,
    target_status: String,
    row_version: i64,
}

impl MergeResult {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "movement_id": self.movement_id,
            "source_pallet_id": self.source_pallet_id,
            "target_pallet_id": self.target_pallet_id,
            "target_status": self.target_status,
            "row_version": self.row_version,
        })
    }
}

#[derive(Debug)]
struct Parsed {
    source_pallet_id: Uuid,
    target_pallet_id: Uuid,
    occurred_at: TimestampTz,
}

fn parse(command: &MergeCommand) -> Result<Parsed, AccessError> {
    let parsed = Parsed {
        source_pallet_id: scalar::uuid("value.source_pallet_id", &command.source_pallet_id)?,
        target_pallet_id: scalar::uuid("value.target_pallet_id", &command.target_pallet_id)?,
        occurred_at: scalar::timestamp("value.occurred_at", &command.occurred_at)?,
    };
    // A pallet merged into itself is refused here; the claim table's check
    // constraint would refuse it too, as an opaque internal_error.
    if parsed.source_pallet_id.0 == parsed.target_pallet_id.0 {
        return Err(AccessError::field(
            AccessErrorKind::InvalidInput,
            "value.target_pallet_id",
        ));
    }
    Ok(parsed)
}

fn canonical_command(command: &MergeCommand, parsed: &Parsed) -> Vec<u8> {
    wamn_execution_contract::canonical_json_bytes(&serde_json::json!({
        "source_pallet_id": parsed.source_pallet_id.0,
        "target_pallet_id": parsed.target_pallet_id.0,
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
pub(crate) async fn execute(command: &MergeCommand) -> Result<MergeResult, AccessError> {
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

/// The target row, once BOTH locked rows are found by id and live: a missing
/// one is the refusal that names it, and a consumed one is not live stock and
/// refuses the same way.
fn locked_target(
    rows: Vec<sql::LockBothPalletsRow>,
    parsed: &Parsed,
) -> Result<sql::LockBothPalletsRow, AccessError> {
    let mut source = None;
    let mut target = None;
    for row in rows {
        if row.id.0 == parsed.source_pallet_id.0 {
            source = Some(row);
        } else if row.id.0 == parsed.target_pallet_id.0 {
            target = Some(row);
        }
    }
    let live = |row: Option<sql::LockBothPalletsRow>, field: &str, id: &Uuid| {
        row.filter(|row| row.status != scalar::CONSUMED)
            .ok_or_else(|| AccessError::missing(AccessErrorKind::PalletNotFound, field, &id.0))
    };
    live(source, "value.source_pallet_id", &parsed.source_pallet_id)?;
    live(target, "value.target_pallet_id", &parsed.target_pallet_id)
}

async fn run(
    transaction: &mut Transaction,
    command: &MergeCommand,
    canonical: &[u8],
    parsed: &Parsed,
) -> Result<MergeResult, AccessError> {
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
        // The target's status was never this command's to change, so the
        // live row's is the original's.
        let rows = sql::lock_both_pallets(
            transaction,
            replay.source_pallet_id.clone(),
            replay.target_pallet_id.clone(),
        )
        .await
        .map_err(|e| error::from_statement(&e))?;
        let target = rows
            .into_iter()
            .find(|row| row.id.0 == replay.target_pallet_id.0)
            .ok_or_else(internal)?;
        return Ok(MergeResult {
            movement_id: replay.movement_id.0,
            source_pallet_id: replay.source_pallet_id.0,
            target_pallet_id: replay.target_pallet_id.0,
            target_status: target.status,
            row_version,
        });
    }

    let claim = sql::claim_command(
        transaction,
        key.clone(),
        canonical.to_vec(),
        parsed.source_pallet_id.clone(),
        parsed.target_pallet_id.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    .ok_or_else(retry)?;

    // THE SERIALIZATION POINT: both rows, in id order.
    let rows = sql::lock_both_pallets(
        transaction,
        parsed.source_pallet_id.clone(),
        parsed.target_pallet_id.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    let target = locked_target(rows, parsed)?;
    if target.row_version != command.expected_row_version {
        return Err(AccessError::conflict(
            command.expected_row_version,
            target.row_version,
        ));
    }

    // EVERY SOURCE ROW LANDS ON THE TARGET, matched by product and status,
    // and each is a movement of its own.
    let quantities = sql::select_source_quantity(transaction, parsed.source_pallet_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?;
    for quantity in &quantities {
        let added = sql::add_to_target(
            transaction,
            parsed.target_pallet_id.clone(),
            quantity.product_id.clone(),
            quantity.status.clone(),
            quantity.quantity.clone(),
        )
        .await
        .map_err(|e| error::from_statement(&e))?;
        if added.is_none() {
            sql::place_on_target(
                transaction,
                parsed.target_pallet_id.clone(),
                quantity.product_id.clone(),
                quantity.status.clone(),
                quantity.quantity.clone(),
            )
            .await
            .map_err(|e| error::from_statement(&e))?;
        }
        sql::insert_movement(
            transaction,
            key.clone(),
            parsed.source_pallet_id.clone(),
            quantity.product_id.clone(),
            quantity.quantity.clone(),
            parsed.occurred_at.clone(),
        )
        .await
        .map_err(|e| error::from_statement(&e))?;
    }

    sql::consume_source(transaction, parsed.source_pallet_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?;
    let touched = sql::touch_target(transaction, parsed.target_pallet_id.clone())
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

    Ok(MergeResult {
        movement_id: claim.movement_id.0,
        source_pallet_id: parsed.source_pallet_id.0.clone(),
        target_pallet_id: parsed.target_pallet_id.0.clone(),
        target_status: touched.status,
        row_version: touched.row_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "00000000-0000-0000-0000-000000000301";
    const TARGET: &str = "00000000-0000-0000-0000-000000000302";

    fn command(source: &str, target: &str) -> MergeCommand {
        MergeCommand {
            idempotency_key: "k".to_owned(),
            source_pallet_id: source.to_owned(),
            target_pallet_id: target.to_owned(),
            expected_row_version: 1,
            occurred_at: "2026-09-05T00:00:00Z".to_owned(),
        }
    }

    fn row(id: &str, status: &str) -> sql::LockBothPalletsRow {
        sql::LockBothPalletsRow {
            id: Uuid(id.to_owned()),
            location_id: Uuid("00000000-0000-0000-0000-000000000201".to_owned()),
            row_version: 1,
            status: status.to_owned(),
        }
    }

    #[test]
    fn a_pallet_merged_into_itself_is_invalid_input() {
        let error = parse(&command(SOURCE, SOURCE)).unwrap_err();
        assert_eq!(error.kind(), AccessErrorKind::InvalidInput);
        assert_eq!(error.detail()["field"], "value.target_pallet_id");
        assert!(parse(&command(SOURCE, TARGET)).is_ok());
    }

    /// The lock answers in id order and may answer with fewer rows than
    /// asked; the pair is found by id, and a consumed pallet is not live.
    #[test]
    fn the_locked_pair_is_found_by_id_and_must_be_live() {
        let parsed = parse(&command(TARGET, SOURCE)).unwrap();
        let target =
            locked_target(vec![row(SOURCE, "available"), row(TARGET, "held")], &parsed).unwrap();
        assert_eq!(target.id.0, SOURCE);

        let parsed = parse(&command(SOURCE, TARGET)).unwrap();
        let missing = locked_target(vec![row(SOURCE, "available")], &parsed).unwrap_err();
        assert_eq!(missing.kind(), AccessErrorKind::PalletNotFound);
        assert_eq!(missing.detail()["field"], "value.target_pallet_id");
        assert_eq!(missing.detail()["id"], TARGET);

        let consumed = locked_target(
            vec![row(SOURCE, "consumed"), row(TARGET, "available")],
            &parsed,
        )
        .unwrap_err();
        assert_eq!(consumed.kind(), AccessErrorKind::PalletNotFound);
        assert_eq!(consumed.detail()["field"], "value.source_pallet_id");
    }
}

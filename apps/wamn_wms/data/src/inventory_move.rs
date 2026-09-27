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
//! → lock the pallet          (the serialization point); a consumed one is not found
//! → compare expected_row_version to observed
//! → validate the destination
//! → write one movement per quantity row
//! → move the pallet and bump its revision
//! ```
//!
//! The generated codec claims the key in the write log and holds the
//! transaction, so a retry answers the stored result. The movement ids are the
//! ids of the movement rows; a pallet with no quantity rows writes none.
//!
//! The lock is on `pallet` and not on `pallet_quantity` deliberately: two
//! concurrent moves of one pallet must not both succeed by touching different
//! quantity rows, and the pallet is what makes them serialize.

use serde::Deserialize;
use wamn_postgres_statements::{TimestampTz, Transaction, Uuid};

use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::inventory_move as sql;
use crate::scalar;

/// One envelope item's command body.
#[derive(Debug, Deserialize)]
pub struct MoveCommand {
    pub pallet_id: String,
    pub to_location_id: String,
    pub expected_row_version: i32,
    pub occurred_at: String,
}

/// What one accepted move answers with.
#[derive(Debug, PartialEq, Eq)]
pub struct MoveResult {
    pub movement_ids: Vec<String>,
    pub pallet_id: String,
    pub location_id: String,
    pub pallet_status: String,
    pub row_version: i32,
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

/// Run one command item in the transaction its codec holds for the write log.
///
/// # Errors
///
/// [`AccessError`] carrying the literal and detail the operation contract
/// declares for that refusal.
pub async fn execute(
    transaction: &mut Transaction,
    command: &MoveCommand,
) -> Result<MoveResult, AccessError> {
    let parsed = parse(command)?;
    run(transaction, command, &parsed).await
}

/// The locked pallet, once it is found and live: a missing one is the
/// refusal that names it, and a consumed one is not live stock and refuses
/// the same way.
fn live_pallet(
    locked: Option<sql::LockPalletRow>,
    pallet_id: &str,
) -> Result<sql::LockPalletRow, AccessError> {
    locked
        .filter(|row| row.status != scalar::CONSUMED)
        .ok_or_else(|| {
            AccessError::missing(
                AccessErrorKind::PalletNotFound,
                "value.pallet_id",
                pallet_id,
            )
        })
}

async fn run(
    transaction: &mut Transaction,
    command: &MoveCommand,
    parsed: &Parsed,
) -> Result<MoveResult, AccessError> {
    // THE SERIALIZATION POINT.
    let locked = live_pallet(
        sql::lock_pallet(transaction, parsed.pallet_id.clone())
            .await
            .map_err(|e| error::from_statement(&e))?,
        &command.pallet_id,
    )?;

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
    let mut movement_ids = Vec::with_capacity(quantities.len());
    for quantity in &quantities {
        let movement = sql::insert_movement(
            transaction,
            parsed.pallet_id.clone(),
            quantity.product_id.clone(),
            locked.location_id.clone(),
            parsed.to_location_id.clone(),
            quantity.quantity.clone(),
            parsed.occurred_at.clone(),
        )
        .await
        .map_err(|e| error::from_statement(&e))?;
        movement_ids.push(movement.id.0);
    }

    let moved = sql::move_pallet(
        transaction,
        parsed.pallet_id.clone(),
        parsed.to_location_id.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;

    Ok(MoveResult {
        movement_ids,
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
            pallet_id: pallet_id.to_owned(),
            to_location_id: "00000000-0000-0000-0000-000000000201".to_owned(),
            expected_row_version: 1,
            occurred_at: occurred_at.to_owned(),
        }
    }

    fn locked(status: &str) -> sql::LockPalletRow {
        sql::LockPalletRow {
            location_id: Uuid("00000000-0000-0000-0000-000000000201".to_owned()),
            row_version: 1,
            status: status.to_owned(),
        }
    }

    /// A consumed pallet is not live stock, so a move refuses it as it
    /// refuses a missing one, on the pallet it names (wamn-prku).
    #[test]
    fn a_consumed_pallet_is_not_found() {
        const PALLET: &str = "00000000-0000-0000-0000-00000000030a";
        for status in ["available", "held"] {
            assert!(
                live_pallet(Some(locked(status)), PALLET).is_ok(),
                "{status}"
            );
        }
        for row in [None, Some(locked("consumed"))] {
            let error = live_pallet(row, PALLET).unwrap_err();
            assert_eq!(error.kind(), AccessErrorKind::PalletNotFound);
            assert_eq!(error.detail()["field"], "value.pallet_id");
            assert_eq!(error.detail()["id"], PALLET);
        }
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

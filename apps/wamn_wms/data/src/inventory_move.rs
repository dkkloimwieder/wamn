//! `inventory.move` — the contended command.
//!
//! One packaging moved between locations, transactionally, with optimistic
//! concurrency at the row that actually contends. The generator
//! wrote every statement and its decoding; what is authored here is the ORDER
//! those statements run in and what each refusal means.
//!
//! # The shape of one item
//!
//! ```text
//! → lock the packaging          (the serialization point); a consumed one is not found
//! → compare expected_row_version to observed
//! → refuse a destination the packaging is already at
//! → validate the destination
//! → move the packaging, stamp located_at, and bump its revision
//! ```
//!
//! A move is a packaging command, not a quantity command: the location change
//! is a fact on `packaging` (`location_id` with the `located_at` the command
//! names) and its record history, and no stock changes packaging or status,
//! so it writes no transaction row. The label workflow
//! triggers on the location change. The generated codec claims the key in the
//! write log and holds the transaction, so a retry answers the stored result.

use serde::Deserialize;
use wamn_postgres_statements::{TimestampTz, Transaction, Uuid};

use crate::error::{self, AccessError, AccessErrorType};
use crate::scalar;
use crate::statements::wamn::inventory_move as sql;

/// One envelope item's command body.
#[derive(Debug, Deserialize)]
pub struct MoveCommand {
    pub packaging_id: String,
    pub to_location_id: String,
    pub expected_row_version: i32,
    pub occurred_at: String,
}

/// What one accepted move answers with.
#[derive(Debug, PartialEq, Eq)]
pub struct MoveResult {
    pub packaging_id: String,
    pub location_id: String,
    pub row_version: i32,
}

/// The command's scalars in their one wire spelling.
#[derive(Debug)]
struct Parsed {
    packaging_id: Uuid,
    to_location_id: Uuid,
    occurred_at: TimestampTz,
}

fn parse(command: &MoveCommand) -> Result<Parsed, AccessError> {
    Ok(Parsed {
        packaging_id: scalar::uuid("value.packaging_id", &command.packaging_id)?,
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

/// The locked packaging, once it is found and live: a missing one is the
/// refusal that names it, and a consumed one is not live stock and refuses
/// the same way.
fn live_packaging(
    locked: Option<sql::LockPackagingRow>,
    packaging_id: &str,
) -> Result<sql::LockPackagingRow, AccessError> {
    locked
        .filter(|row| row.status != scalar::CONSUMED)
        .ok_or_else(|| {
            AccessError::missing(
                AccessErrorType::PackagingNotFound,
                "value.packaging_id",
                packaging_id,
            )
        })
}

async fn run(
    transaction: &mut Transaction,
    command: &MoveCommand,
    parsed: &Parsed,
) -> Result<MoveResult, AccessError> {
    // THE SERIALIZATION POINT.
    let locked = live_packaging(
        sql::lock_packaging(transaction, parsed.packaging_id.clone())
            .await
            .map_err(|e| error::from_statement(&e))?,
        &command.packaging_id,
    )?;

    if locked.row_version != command.expected_row_version {
        return Err(AccessError::conflict(
            command.expected_row_version,
            locked.row_version,
        ));
    }

    // Nothing to move: the packaging is already at the destination.
    if locked.location_id == parsed.to_location_id {
        return Err(AccessError::field(
            AccessErrorType::InvalidInput,
            "value.to_location_id",
        ));
    }

    sql::validate_location(transaction, parsed.to_location_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(|| {
            AccessError::missing(
                AccessErrorType::LocationNotFound,
                "value.to_location_id",
                &command.to_location_id,
            )
        })?;

    let moved = sql::move_packaging(
        transaction,
        parsed.packaging_id.clone(),
        parsed.to_location_id.clone(),
        parsed.occurred_at.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;

    Ok(MoveResult {
        packaging_id: command.packaging_id.clone(),
        location_id: moved.location_id.0.clone(),
        row_version: moved.row_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(packaging_id: &str, occurred_at: &str) -> MoveCommand {
        MoveCommand {
            packaging_id: packaging_id.to_owned(),
            to_location_id: "00000000-0000-0000-0000-000000000201".to_owned(),
            expected_row_version: 1,
            occurred_at: occurred_at.to_owned(),
        }
    }

    fn locked(status: &str) -> sql::LockPackagingRow {
        sql::LockPackagingRow {
            location_id: Uuid("00000000-0000-0000-0000-000000000201".to_owned()),
            row_version: 1,
            status: status.to_owned(),
        }
    }

    /// A consumed packaging is not live stock, so a move refuses it as it
    /// refuses a missing one, on the packaging it names (wamn-prku).
    #[test]
    fn a_consumed_packaging_is_not_found() {
        const PACKAGING: &str = "00000000-0000-0000-0000-00000000030a";
        for status in ["available", "held"] {
            assert!(
                live_packaging(Some(locked(status)), PACKAGING).is_ok(),
                "{status}"
            );
        }
        for row in [None, Some(locked("consumed"))] {
            let error = live_packaging(row, PACKAGING).unwrap_err();
            assert_eq!(error.error_type(), AccessErrorType::PackagingNotFound);
            assert_eq!(error.detail()["field"], "value.packaging_id");
            assert_eq!(error.detail()["id"], PACKAGING);
        }
    }

    #[test]
    fn an_unspellable_scalar_is_refused_before_any_statement() {
        let mut packaging = command("not-a-uuid", "2026-09-05T00:00:00Z");
        assert_eq!(
            parse(&packaging).unwrap_err().detail()["field"],
            "value.packaging_id"
        );
        packaging = command("00000000-0000-0000-0000-00000000030a", "yesterday");
        assert_eq!(
            parse(&packaging).unwrap_err().detail()["field"],
            "value.occurred_at"
        );
    }
}

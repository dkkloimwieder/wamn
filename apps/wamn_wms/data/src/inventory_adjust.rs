//! `inventory.adjust` -- a counted correction to one quantity row.
//!
//! ```text
//! → lock the pallet          (the serialization point)
//! → compare expected_row_version to observed
//! → set the (pallet, product, status) row to the counted quantity
//! → write the movement, with its reason
//! → bump the pallet's revision
//! ```
//!
//! The movement records the quantity the row BECAME, not a delta: an adjust
//! is a count, and the history keeps what was counted. The generated codec
//! claims the key in the write log and holds the transaction, so a retry
//! answers the stored result. The one movement id is the id of the movement row.

use serde::Deserialize;
use wamn_postgres_statements::{Numeric, TimestampTz, Transaction, Uuid};

use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::inventory_adjust as sql;
use crate::scalar;

/// One envelope item's command body.
#[derive(Debug, Deserialize)]
pub struct AdjustCommand {
    pub pallet_id: String,
    pub product_id: String,
    pub status: String,
    pub quantity: String,
    pub reason_code: String,
    pub expected_row_version: i32,
    pub occurred_at: String,
}

/// What one accepted adjust answers with.
#[derive(Debug, PartialEq, Eq)]
pub struct AdjustResult {
    pub movement_ids: Vec<String>,
    pub pallet_id: String,
    pub adjusted_quantity: String,
    pub pallet_status: String,
    pub row_version: i32,
}

/// The command's scalars in their one wire spelling.
#[derive(Debug)]
struct Parsed {
    pallet_id: Uuid,
    product_id: Uuid,
    status: String,
    quantity: Numeric,
    occurred_at: TimestampTz,
}

fn parse(command: &AdjustCommand) -> Result<Parsed, AccessError> {
    if command.reason_code.is_empty() {
        return Err(AccessError::field(
            AccessErrorKind::InvalidInput,
            "value.reason_code",
        ));
    }
    Ok(Parsed {
        pallet_id: scalar::uuid("value.pallet_id", &command.pallet_id)?,
        product_id: scalar::uuid("value.product_id", &command.product_id)?,
        status: scalar::quantity_status("value.status", &command.status)?,
        quantity: scalar::numeric("value.quantity", &command.quantity)?,
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
    command: &AdjustCommand,
) -> Result<AdjustResult, AccessError> {
    let parsed = parse(command)?;
    run(transaction, command, &parsed).await
}

async fn run(
    transaction: &mut Transaction,
    command: &AdjustCommand,
    parsed: &Parsed,
) -> Result<AdjustResult, AccessError> {
    // THE SERIALIZATION POINT.
    let not_found = || {
        AccessError::missing(
            AccessErrorKind::PalletNotFound,
            "value.pallet_id",
            &parsed.pallet_id.0,
        )
    };
    let locked = sql::lock_pallet(transaction, parsed.pallet_id.clone())
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

    let set = sql::set_quantity(
        transaction,
        parsed.pallet_id.clone(),
        parsed.product_id.clone(),
        parsed.status.clone(),
        parsed.quantity.clone(),
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

    let movement = sql::insert_movement(
        transaction,
        parsed.pallet_id.clone(),
        parsed.product_id.clone(),
        set.quantity.clone(),
        command.reason_code.clone(),
        parsed.occurred_at.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;

    let touched = sql::touch_pallet(transaction, parsed.pallet_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?;

    Ok(AdjustResult {
        movement_ids: vec![movement.id.0],
        pallet_id: parsed.pallet_id.0.clone(),
        adjusted_quantity: set.quantity.0,
        pallet_status: touched.status,
        row_version: touched.row_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(pallet_id: &str, occurred_at: &str) -> AdjustCommand {
        AdjustCommand {
            pallet_id: pallet_id.to_owned(),
            product_id: "00000000-0000-0000-0000-000000000101".to_owned(),
            status: "available".to_owned(),
            quantity: "7".to_owned(),
            reason_code: "cycle-count".to_owned(),
            expected_row_version: 1,
            occurred_at: occurred_at.to_owned(),
        }
    }

    #[test]
    fn the_reason_and_status_are_refused_before_any_statement() {
        let mut blank = command(
            "00000000-0000-0000-0000-00000000030a",
            "2026-09-05T00:00:00Z",
        );
        blank.reason_code.clear();
        assert_eq!(
            parse(&blank).unwrap_err().detail()["field"],
            "value.reason_code"
        );
        let mut consumed = command(
            "00000000-0000-0000-0000-00000000030a",
            "2026-09-05T00:00:00Z",
        );
        consumed.status = "consumed".to_owned();
        assert_eq!(
            parse(&consumed).unwrap_err().detail()["field"],
            "value.status"
        );
    }
}

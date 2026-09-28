//! `inventory.split` -- part of one quantity row moved onto a NEW packaging.
//!
//! ```text
//! → lock the source packaging     (the serialization point)
//! → compare expected_row_version to observed
//! → validate the destination
//! → read the quantity row, then take from it (it must keep stock)
//! → create the new packaging, place the quantity on it, write the transaction
//! → bump the source's revision
//! ```
//!
//! The generated codec claims the key in the write log and holds the
//! transaction, so a retry answers the stored result and creates no second
//! packaging. The new packaging's id and the one transaction id come from their
//! inserts' `RETURNING`. The new packaging takes the type the command names,
//! which need not be the source's, and inherits the source's status -- a split
//! of a held packaging does not release the hold. The source must keep stock.

use serde::Deserialize;
use wamn_postgres_statements::{Numeric, TimestampTz, Transaction, Uuid};

use crate::error::{self, AccessError, AccessErrorKind};
use crate::scalar;
use crate::statements::wamn::inventory_split as sql;

/// One envelope item's command body.
#[derive(Debug, Deserialize)]
pub struct SplitCommand {
    pub source_packaging_id: String,
    pub product_id: String,
    pub status: String,
    pub quantity: String,
    pub new_packaging_code: String,
    pub new_packaging_type: String,
    pub to_location_id: String,
    pub expected_row_version: i32,
    pub occurred_at: String,
}

/// What one accepted split answers with.
#[derive(Debug, PartialEq, Eq)]
pub struct SplitResult {
    pub transaction_ids: Vec<String>,
    pub source_packaging_id: String,
    pub new_packaging_id: String,
    pub source_status: String,
    pub row_version: i32,
}

#[derive(Debug)]
struct Parsed {
    source_packaging_id: Uuid,
    product_id: Uuid,
    status: String,
    quantity: Numeric,
    new_packaging_type: String,
    to_location_id: Uuid,
    occurred_at: TimestampTz,
}

fn parse(command: &SplitCommand) -> Result<Parsed, AccessError> {
    if command.new_packaging_code.is_empty() {
        return Err(AccessError::field(
            AccessErrorKind::InvalidInput,
            "value.new_packaging_code",
        ));
    }
    Ok(Parsed {
        source_packaging_id: scalar::uuid(
            "value.source_packaging_id",
            &command.source_packaging_id,
        )?,
        product_id: scalar::uuid("value.product_id", &command.product_id)?,
        status: scalar::quantity_status("value.status", &command.status)?,
        quantity: scalar::numeric("value.quantity", &command.quantity)?,
        new_packaging_type: scalar::packaging_type(
            "value.new_packaging_type",
            &command.new_packaging_type,
        )?,
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
    command: &SplitCommand,
) -> Result<SplitResult, AccessError> {
    let parsed = parse(command)?;
    run(transaction, command, &parsed).await
}

async fn run(
    transaction: &mut Transaction,
    command: &SplitCommand,
    parsed: &Parsed,
) -> Result<SplitResult, AccessError> {
    // THE SERIALIZATION POINT.
    let not_found = || {
        AccessError::missing(
            AccessErrorKind::PackagingNotFound,
            "value.source_packaging_id",
            &parsed.source_packaging_id.0,
        )
    };
    let locked = sql::lock_packaging(transaction, parsed.source_packaging_id.clone())
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
        parsed.source_packaging_id.clone(),
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
        parsed.source_packaging_id.clone(),
        parsed.product_id.clone(),
        parsed.status.clone(),
        parsed.quantity.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    .ok_or_else(|| AccessError::insufficient("value.quantity", &held.quantity.0))?;

    let created = sql::create_packaging(
        transaction,
        command.new_packaging_code.clone(),
        parsed.new_packaging_type.clone(),
        parsed.to_location_id.clone(),
        parsed.occurred_at.clone(),
        locked.status.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    sql::place_quantity(
        transaction,
        created.id.clone(),
        parsed.product_id.clone(),
        parsed.status.clone(),
        parsed.quantity.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    let written = sql::insert_transaction(
        transaction,
        parsed.product_id.clone(),
        parsed.quantity.clone(),
        parsed.source_packaging_id.clone(),
        parsed.status.clone(),
        created.id.clone(),
        parsed.occurred_at.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;

    let touched = sql::touch_source(transaction, parsed.source_packaging_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?;

    Ok(SplitResult {
        transaction_ids: vec![written.id.0],
        source_packaging_id: parsed.source_packaging_id.0.clone(),
        new_packaging_id: created.id.0,
        source_status: touched.status,
        row_version: touched.row_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command() -> SplitCommand {
        SplitCommand {
            source_packaging_id: "00000000-0000-0000-0000-000000000301".to_owned(),
            product_id: "00000000-0000-0000-0000-000000000101".to_owned(),
            status: "available".to_owned(),
            quantity: "4".to_owned(),
            new_packaging_code: "PAL-302".to_owned(),
            new_packaging_type: "tote".to_owned(),
            to_location_id: "00000000-0000-0000-0000-000000000202".to_owned(),
            expected_row_version: 1,
            occurred_at: "2026-09-05T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn a_blank_packaging_code_and_a_zero_quantity_refuse_before_any_statement() {
        let mut blank = command();
        blank.new_packaging_code.clear();
        assert_eq!(
            parse(&blank).unwrap_err().detail()["field"],
            "value.new_packaging_code"
        );
        let mut zero = command();
        zero.quantity = "0.0".to_owned();
        assert_eq!(
            parse(&zero).unwrap_err().detail()["field"],
            "value.quantity"
        );
    }
}

//! `inventory.split` validates and writes under packaging locks.
//!
//! The first statement locks the parent packaging rows and returns any parent
//! refusal. The second reads quantities with a fresh READ COMMITTED snapshot,
//! validates them, and writes through `RETURNING` dependencies. A locking CTE
//! alone cannot refresh the snapshot after a lock wait.
//!
//! The generated codec holds the transaction and write log for exact replay.

use serde::Deserialize;
use wamn_postgres_statements::{Numeric, TimestampTz, Transaction, Uuid};

use crate::error::{self, AccessError, AccessErrorType};
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
            AccessErrorType::InvalidInput,
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
    let locked = sql::lock_packaging(
        transaction,
        parsed.source_packaging_id.clone(),
        command.expected_row_version,
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    error::command_outcome(&locked.outcome, &locked.detail.0)?;

    // This statement starts after the lock wait, so child reads see fresh data.
    let written = sql::split(
        transaction,
        parsed.source_packaging_id.clone(),
        parsed.product_id.clone(),
        parsed.status.clone(),
        parsed.quantity.clone(),
        command.new_packaging_code.clone(),
        parsed.new_packaging_type.clone(),
        parsed.to_location_id.clone(),
        parsed.occurred_at.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    error::command_outcome(&written.outcome, &written.detail.0)?;
    let (Some(transaction_id), Some(new_packaging_id), Some(row_version), Some(source_status)) = (
        written.transaction_id,
        written.new_packaging_id,
        written.row_version,
        written.status,
    ) else {
        return Err(AccessError::new(
            AccessErrorType::InternalError,
            serde_json::json!({}),
        ));
    };
    Ok(SplitResult {
        transaction_ids: vec![transaction_id.0],
        source_packaging_id: parsed.source_packaging_id.0.clone(),
        new_packaging_id: new_packaging_id.0,
        source_status,
        row_version,
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

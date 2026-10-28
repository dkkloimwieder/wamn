//! `inventory.adjust` validates and writes under packaging locks.
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
use crate::statements::wamn::inventory_adjust as sql;

/// One envelope item's command body.
#[derive(Debug, Deserialize)]
pub struct AdjustCommand {
    pub packaging_id: String,
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
    pub transaction_ids: Vec<String>,
    pub packaging_id: String,
    pub adjusted_quantity: String,
    pub packaging_status: String,
    pub row_version: i32,
}

/// The command's scalars in their one wire spelling.
#[derive(Debug)]
struct Parsed {
    packaging_id: Uuid,
    product_id: Uuid,
    status: String,
    quantity: Numeric,
    occurred_at: TimestampTz,
}

fn parse(command: &AdjustCommand) -> Result<Parsed, AccessError> {
    if command.reason_code.is_empty() {
        return Err(AccessError::field(
            AccessErrorType::InvalidInput,
            "value.reason_code",
        ));
    }
    Ok(Parsed {
        packaging_id: scalar::uuid("value.packaging_id", &command.packaging_id)?,
        product_id: scalar::uuid("value.product_id", &command.product_id)?,
        status: scalar::quantity_status("value.status", &command.status)?,
        quantity: scalar::count("value.quantity", &command.quantity)?,
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
    let locked = sql::lock_packaging(
        transaction,
        parsed.packaging_id.clone(),
        command.expected_row_version,
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    error::command_outcome(&locked.outcome, &locked.detail.0)?;

    // This statement starts after the lock wait, so child reads see fresh data.
    let written = sql::adjust(
        transaction,
        parsed.packaging_id.clone(),
        parsed.product_id.clone(),
        parsed.status.clone(),
        parsed.quantity.clone(),
        command.reason_code.clone(),
        parsed.occurred_at.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    error::command_outcome(&written.outcome, &written.detail.0)?;
    let (Some(transaction_id), Some(adjusted_quantity), Some(row_version), Some(packaging_status)) = (
        written.transaction_id,
        written.adjusted_quantity,
        written.row_version,
        written.status,
    ) else {
        return Err(AccessError::new(
            AccessErrorType::InternalError,
            serde_json::json!({}),
        ));
    };
    Ok(AdjustResult {
        transaction_ids: vec![transaction_id.0],
        packaging_id: parsed.packaging_id.0.clone(),
        adjusted_quantity: adjusted_quantity.0,
        packaging_status,
        row_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(packaging_id: &str, occurred_at: &str) -> AdjustCommand {
        AdjustCommand {
            packaging_id: packaging_id.to_owned(),
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

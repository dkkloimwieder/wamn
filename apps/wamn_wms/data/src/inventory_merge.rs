//! `inventory.merge` validates and writes under packaging locks.
//!
//! The first statement locks the parent packaging rows and returns any parent
//! refusal. The second reads quantities with a fresh READ COMMITTED snapshot,
//! validates them, and writes through `RETURNING` dependencies. A locking CTE
//! alone cannot refresh the snapshot after a lock wait.
//!
//! The generated codec holds the transaction and write log for exact replay.

use serde::Deserialize;
use wamn_postgres_statements::{TimestampTz, Transaction, Uuid};

use crate::error::{self, AccessError, AccessErrorType};
use crate::scalar;
use crate::statements::wamn::inventory_merge as sql;

/// One request element's command body.
#[derive(Debug, Deserialize)]
pub struct MergeCommand {
    pub source_packaging_id: String,
    pub target_packaging_id: String,
    pub expected_row_version: i32,
    pub occurred_at: String,
}

/// What one accepted merge answers with.
#[derive(Debug, PartialEq, Eq)]
pub struct MergeResult {
    pub transaction_ids: Vec<String>,
    pub source_packaging_id: String,
    pub target_packaging_id: String,
    pub target_status: String,
    pub row_version: i32,
}

#[derive(Debug)]
struct Parsed {
    source_packaging_id: Uuid,
    target_packaging_id: Uuid,
    occurred_at: TimestampTz,
}

fn parse(command: &MergeCommand) -> Result<Parsed, AccessError> {
    let parsed = Parsed {
        source_packaging_id: scalar::uuid(
            "value.source_packaging_id",
            &command.source_packaging_id,
        )?,
        target_packaging_id: scalar::uuid(
            "value.target_packaging_id",
            &command.target_packaging_id,
        )?,
        occurred_at: scalar::timestamp("value.occurred_at", &command.occurred_at)?,
    };
    // A packaging merged into itself is refused here, before any statement.
    if parsed.source_packaging_id.0 == parsed.target_packaging_id.0 {
        return Err(AccessError::field(
            AccessErrorType::InvalidInput,
            "value.target_packaging_id",
        ));
    }
    Ok(parsed)
}

/// Run one command request element in the transaction its codec holds for the write log.
///
/// # Errors
///
/// [`AccessError`] carrying the literal and detail the operation contract
/// declares for that refusal.
pub async fn execute(
    transaction: &mut Transaction,
    command: &MergeCommand,
) -> Result<MergeResult, AccessError> {
    let parsed = parse(command)?;
    run(transaction, command, &parsed).await
}

async fn run(
    transaction: &mut Transaction,
    command: &MergeCommand,
    parsed: &Parsed,
) -> Result<MergeResult, AccessError> {
    let locked = sql::lock_both_packagings(
        transaction,
        parsed.source_packaging_id.clone(),
        parsed.target_packaging_id.clone(),
        command.expected_row_version,
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    error::command_outcome(&locked.outcome, &locked.detail.0)?;

    // This statement starts after the lock wait, so child reads see fresh data.
    let written = sql::merge(
        transaction,
        parsed.source_packaging_id.clone(),
        parsed.target_packaging_id.clone(),
        parsed.occurred_at.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    error::command_outcome(&written.outcome, &written.detail.0)?;
    let (Some(row_version), Some(target_status)) = (written.row_version, written.status) else {
        return Err(AccessError::new(
            AccessErrorType::InternalError,
            serde_json::json!({}),
        ));
    };
    let transaction_ids = serde_json::from_str(&written.transaction_ids.0)
        .map_err(|_| AccessError::new(AccessErrorType::InternalError, serde_json::json!({})))?;
    Ok(MergeResult {
        transaction_ids,
        source_packaging_id: parsed.source_packaging_id.0.clone(),
        target_packaging_id: parsed.target_packaging_id.0.clone(),
        target_status,
        row_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "00000000-0000-0000-0000-000000000301";
    const TARGET: &str = "00000000-0000-0000-0000-000000000302";

    fn command(source: &str, target: &str) -> MergeCommand {
        MergeCommand {
            source_packaging_id: source.to_owned(),
            target_packaging_id: target.to_owned(),
            expected_row_version: 1,
            occurred_at: "2026-09-05T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn a_packaging_merged_into_itself_is_invalid_input() {
        let error = parse(&command(SOURCE, SOURCE)).unwrap_err();
        assert_eq!(error.error_type(), AccessErrorType::InvalidInput);
        assert_eq!(error.detail()["field"], "value.target_packaging_id");
        assert!(parse(&command(SOURCE, TARGET)).is_ok());
    }
}

//! `inventory.merge` -- one packaging absorbed into another.
//!
//! ```text
//! → lock BOTH packagings, in id order   (the serialization point)
//! → compare expected_row_version to the target's
//! → for each source quantity row: add it to the target's matching row,
//!   or place a new one, and write a transaction from source to target
//! → delete the source balance rows, and consume the source
//! → bump the target's revision
//! ```
//!
//! The generated codec claims the key in the write log and holds the
//! transaction, so a retry answers the stored result. The transaction ids are
//! the ids of the rows it wrote, one for each source quantity row. A source
//! with no quantity rows has nothing to merge and refuses. The two packagings
//! need not share a type.
//!
//! The revision the caller names is the TARGET's: that is the packaging the
//! command answers with and the one whose stock changes. The source only has
//! to be live, and once consumed it can never be merged again, so a stale
//! view of it has nothing to race. Both are locked in id order -- two merges
//! naming one pair in opposite orders cannot deadlock (`lock_both_packagings.sql`).
//! Each transaction row names both packagings.

use serde::Deserialize;
use wamn_postgres_statements::{TimestampTz, Transaction, Uuid};

use crate::error::{self, AccessError, AccessErrorKind};
use crate::scalar;
use crate::statements::wamn::inventory_merge as sql;

/// One envelope item's command body.
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
            AccessErrorKind::InvalidInput,
            "value.target_packaging_id",
        ));
    }
    Ok(parsed)
}

/// Run one command item in the transaction its codec holds for the write log.
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

/// The target row, once BOTH locked rows are found by id and live: a missing
/// one is the refusal that names it, and a consumed one is not live stock and
/// refuses the same way.
fn locked_target(
    rows: Vec<sql::LockBothPackagingsRow>,
    parsed: &Parsed,
) -> Result<sql::LockBothPackagingsRow, AccessError> {
    let mut source = None;
    let mut target = None;
    for row in rows {
        if row.id.0 == parsed.source_packaging_id.0 {
            source = Some(row);
        } else if row.id.0 == parsed.target_packaging_id.0 {
            target = Some(row);
        }
    }
    let live = |row: Option<sql::LockBothPackagingsRow>, field: &str, id: &Uuid| {
        row.filter(|row| row.status != scalar::CONSUMED)
            .ok_or_else(|| AccessError::missing(AccessErrorKind::PackagingNotFound, field, &id.0))
    };
    live(
        source,
        "value.source_packaging_id",
        &parsed.source_packaging_id,
    )?;
    live(
        target,
        "value.target_packaging_id",
        &parsed.target_packaging_id,
    )
}

async fn run(
    transaction: &mut Transaction,
    command: &MergeCommand,
    parsed: &Parsed,
) -> Result<MergeResult, AccessError> {
    // THE SERIALIZATION POINT: both rows, in id order.
    let rows = sql::lock_both_packagings(
        transaction,
        parsed.source_packaging_id.clone(),
        parsed.target_packaging_id.clone(),
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
    // and each is a transaction of its own.
    let quantities = sql::select_source_quantity(transaction, parsed.source_packaging_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?;
    if quantities.is_empty() {
        return Err(AccessError::field(
            AccessErrorKind::InvalidInput,
            "value.source_packaging_id",
        ));
    }
    let mut transaction_ids = Vec::with_capacity(quantities.len());
    for quantity in &quantities {
        let added = sql::add_to_target(
            transaction,
            parsed.target_packaging_id.clone(),
            quantity.product_id.clone(),
            quantity.status.clone(),
            quantity.quantity.clone(),
        )
        .await
        .map_err(|e| error::from_statement(&e))?;
        if added.is_none() {
            sql::place_on_target(
                transaction,
                parsed.target_packaging_id.clone(),
                quantity.product_id.clone(),
                quantity.status.clone(),
                quantity.quantity.clone(),
            )
            .await
            .map_err(|e| error::from_statement(&e))?;
        }
        let written = sql::insert_transaction(
            transaction,
            quantity.product_id.clone(),
            quantity.quantity.clone(),
            parsed.source_packaging_id.clone(),
            quantity.status.clone(),
            parsed.target_packaging_id.clone(),
            parsed.occurred_at.clone(),
        )
        .await
        .map_err(|e| error::from_statement(&e))?;
        transaction_ids.push(written.id.0);
    }

    // The whole source moved, so its balance rows are gone.
    sql::delete_source_quantity(transaction, parsed.source_packaging_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?;
    sql::consume_source(transaction, parsed.source_packaging_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?;
    let touched = sql::touch_target(transaction, parsed.target_packaging_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?;

    Ok(MergeResult {
        transaction_ids,
        source_packaging_id: parsed.source_packaging_id.0.clone(),
        target_packaging_id: parsed.target_packaging_id.0.clone(),
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
            source_packaging_id: source.to_owned(),
            target_packaging_id: target.to_owned(),
            expected_row_version: 1,
            occurred_at: "2026-09-05T00:00:00Z".to_owned(),
        }
    }

    fn row(id: &str, status: &str) -> sql::LockBothPackagingsRow {
        sql::LockBothPackagingsRow {
            id: Uuid(id.to_owned()),
            location_id: Uuid("00000000-0000-0000-0000-000000000201".to_owned()),
            row_version: 1,
            status: status.to_owned(),
        }
    }

    #[test]
    fn a_packaging_merged_into_itself_is_invalid_input() {
        let error = parse(&command(SOURCE, SOURCE)).unwrap_err();
        assert_eq!(error.kind(), AccessErrorKind::InvalidInput);
        assert_eq!(error.detail()["field"], "value.target_packaging_id");
        assert!(parse(&command(SOURCE, TARGET)).is_ok());
    }

    /// The lock answers in id order and may answer with fewer rows than
    /// asked; the pair is found by id, and a consumed packaging is not live.
    #[test]
    fn the_locked_pair_is_found_by_id_and_must_be_live() {
        let parsed = parse(&command(TARGET, SOURCE)).unwrap();
        let target =
            locked_target(vec![row(SOURCE, "available"), row(TARGET, "held")], &parsed).unwrap();
        assert_eq!(target.id.0, SOURCE);

        let parsed = parse(&command(SOURCE, TARGET)).unwrap();
        let missing = locked_target(vec![row(SOURCE, "available")], &parsed).unwrap_err();
        assert_eq!(missing.kind(), AccessErrorKind::PackagingNotFound);
        assert_eq!(missing.detail()["field"], "value.target_packaging_id");
        assert_eq!(missing.detail()["id"], TARGET);

        let consumed = locked_target(
            vec![row(SOURCE, "consumed"), row(TARGET, "available")],
            &parsed,
        )
        .unwrap_err();
        assert_eq!(consumed.kind(), AccessErrorKind::PackagingNotFound);
        assert_eq!(consumed.detail()["field"], "value.source_packaging_id");
    }
}

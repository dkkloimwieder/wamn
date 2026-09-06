//! `inventory.adjust` -- a counted correction to one quantity row.
//!
//! ```text
//! canonicalize the body
//! → find a replay: same key ⇒ return the ORIGINAL result, unchanged
//! → claim the key, which pre-generates the movement id
//! → lock the pallet          (the serialization point)
//! → compare expected_row_version to observed
//! → set the (pallet, product, status) row to the counted quantity
//! → write the movement, with its reason
//! → bump the pallet's revision
//! → finalize the claim with the result
//! ```
//!
//! The movement records the quantity the row BECAME, not a delta: an adjust
//! is a count, and the history keeps what was counted. The pallet's status is
//! not this command's to change, so a replay reads it as the live row carries
//! it; the claim row keeps what this command decided -- the quantity and the
//! revision.

use serde::Deserialize;
use wamn_postgres_statements::{Connection, Numeric, TimestampTz, Transaction, Uuid};

use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::inventory_adjust as sql;
use crate::scalar;

pub(crate) const REFUSALS: &[AccessErrorKind] = &[
    AccessErrorKind::InvalidInput,
    AccessErrorKind::PalletNotFound,
    AccessErrorKind::QuantityNotFound,
    AccessErrorKind::ConcurrencyConflict,
    AccessErrorKind::IdempotencyConflict,
    AccessErrorKind::Retry,
    AccessErrorKind::Timeout,
    AccessErrorKind::PermissionDenied,
    AccessErrorKind::InternalError,
];

/// One envelope item's command body.
#[derive(Debug, Deserialize)]
pub(crate) struct AdjustCommand {
    idempotency_key: String,
    pallet_id: String,
    product_id: String,
    status: String,
    quantity: String,
    reason_code: String,
    expected_row_version: i64,
    occurred_at: String,
}

/// What one accepted adjust answers with.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AdjustResult {
    movement_id: String,
    pallet_id: String,
    adjusted_quantity: String,
    pallet_status: String,
    row_version: i64,
}

impl AdjustResult {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "movement_id": self.movement_id,
            "pallet_id": self.pallet_id,
            "adjusted_quantity": self.adjusted_quantity,
            "pallet_status": self.pallet_status,
            "row_version": self.row_version,
        })
    }
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

/// The bytes the idempotency key keys: the RE-SPELLED command, so two
/// deliveries of one count canonicalize alike whatever case or offset each
/// was written in. The key and `request_id` are excluded.
fn canonical_command(command: &AdjustCommand, parsed: &Parsed) -> Vec<u8> {
    wamn_execution_contract::canonical_json_bytes(&serde_json::json!({
        "pallet_id": parsed.pallet_id.0,
        "product_id": parsed.product_id.0,
        "status": parsed.status,
        "quantity": parsed.quantity.0,
        "reason_code": command.reason_code,
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
pub(crate) async fn execute(command: &AdjustCommand) -> Result<AdjustResult, AccessError> {
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

async fn run(
    transaction: &mut Transaction,
    command: &AdjustCommand,
    canonical: &[u8],
    parsed: &Parsed,
) -> Result<AdjustResult, AccessError> {
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
        let (Some(adjusted_quantity), Some(row_version)) =
            (replay.adjusted_quantity, replay.row_version)
        else {
            return Err(retry());
        };
        // The status was never this command's to change, so the live row's
        // is the original's. A pallet cannot vanish (nothing deletes one).
        let pallet = sql::lock_pallet(transaction, replay.pallet_id.clone())
            .await
            .map_err(|e| error::from_statement(&e))?
            .ok_or_else(|| {
                AccessError::new(AccessErrorKind::InternalError, serde_json::json!({}))
            })?;
        return Ok(AdjustResult {
            movement_id: replay.movement_id.0,
            pallet_id: replay.pallet_id.0,
            adjusted_quantity: adjusted_quantity.0,
            pallet_status: pallet.status,
            row_version,
        });
    }

    let claim = sql::claim_command(
        transaction,
        key.clone(),
        canonical.to_vec(),
        parsed.pallet_id.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    .ok_or_else(retry)?;

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

    sql::insert_movement(
        transaction,
        key.clone(),
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

    let finalized = sql::finalize_command(
        transaction,
        key,
        canonical.to_vec(),
        claim.movement_id.clone(),
        set.quantity.clone(),
        touched.row_version,
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    if finalized.row_version.is_none() {
        return Err(retry());
    }

    Ok(AdjustResult {
        movement_id: claim.movement_id.0,
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
            idempotency_key: "k".to_owned(),
            pallet_id: pallet_id.to_owned(),
            product_id: "00000000-0000-0000-0000-000000000101".to_owned(),
            status: "available".to_owned(),
            quantity: "7".to_owned(),
            reason_code: "cycle-count".to_owned(),
            expected_row_version: 1,
            occurred_at: occurred_at.to_owned(),
        }
    }

    /// Two spellings of one count are ONE command under the key: the
    /// canonical bytes come from the respelled scalars, not the caller's.
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
        let bytes = |command: &AdjustCommand| canonical_command(command, &parse(command).unwrap());
        assert_eq!(bytes(&upper), bytes(&lower));
        assert_eq!(bytes(&lower), bytes(&other_key));
        assert!(
            !String::from_utf8(bytes(&lower))
                .unwrap()
                .contains("idempotency_key")
        );
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

//! `appointment.book` -- one carrier, one dock, one slot.
//!
//! # The shape of one item
//!
//! ```text
//! canonicalize the body
//! → find a replay: same key ⇒ return the ORIGINAL result, unchanged
//! → claim the key, which pre-generates the appointment id
//! → lock the dock          (the serialization point)
//! → check the carrier exists
//! → probe the dock's day for an overlapping slot
//! → insert the appointment
//! → finalize the claim with the result
//! ```
//!
//! The lock is on `dock` and not on `appointment`, because the row that must
//! not be raced is the one that does not exist yet. Two concurrent bookings on
//! one dock cannot both pass the overlap probe: the second waits on the dock
//! row until the first commits, then sees the row the first wrote. That is why
//! the database never holds an overlapping pair.

use serde::Deserialize;
use wamn_postgres_statements::{Connection, TimestampTz, Transaction, Uuid};

use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::appointment_book as sql;
use crate::scalar;

pub(crate) const REFUSALS: &[AccessErrorKind] = &[
    AccessErrorKind::InvalidInput,
    AccessErrorKind::CarrierNotFound,
    AccessErrorKind::DockNotFound,
    AccessErrorKind::SlotUnavailable,
    AccessErrorKind::IdempotencyConflict,
    AccessErrorKind::Retry,
    AccessErrorKind::Timeout,
    AccessErrorKind::PermissionDenied,
    AccessErrorKind::InternalError,
];

/// One envelope item's command body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BookCommand {
    pub(crate) idempotency_key: String,
    pub(crate) carrier_id: String,
    pub(crate) dock_id: String,
    pub(crate) slot_start: String,
    pub(crate) slot_end: String,
}

/// The command's scalars in their one wire spelling.
#[derive(Debug)]
struct Parsed {
    carrier_id: Uuid,
    dock_id: Uuid,
    slot_start: TimestampTz,
    slot_end: TimestampTz,
}

fn parse(command: &BookCommand) -> Result<Parsed, AccessError> {
    let parsed = Parsed {
        carrier_id: scalar::uuid("value.carrier_id", &command.carrier_id)?,
        dock_id: scalar::uuid("value.dock_id", &command.dock_id)?,
        slot_start: scalar::timestamp("value.slot_start", &command.slot_start)?,
        slot_end: scalar::timestamp("value.slot_end", &command.slot_end)?,
    };
    // An empty or reversed slot is refused here rather than by the table's
    // check constraint, whose violation the contract can only report as
    // `internal_error`. Both sides are already respelled to one fixed-width
    // UTC form, so their text order IS their time order.
    if parsed.slot_end.0 <= parsed.slot_start.0 {
        return Err(AccessError::field(
            AccessErrorKind::InvalidInput,
            "value.slot_end",
        ));
    }
    Ok(parsed)
}

/// The bytes the idempotency key keys: the RE-SPELLED command, so two
/// deliveries of one booking canonicalize alike whatever case or offset each
/// was written in.
fn canonical_command(parsed: &Parsed) -> Vec<u8> {
    wamn_execution_contract::canonical_json_bytes(&serde_json::json!({
        "carrier_id": parsed.carrier_id.0,
        "dock_id": parsed.dock_id.0,
        "slot_start": parsed.slot_start.0,
        "slot_end": parsed.slot_end.0,
    }))
}

/// # Errors
///
/// [`AccessError`] carrying the literal and detail the operation contract
/// declares for that refusal.
pub(crate) async fn execute(command: &BookCommand) -> Result<serde_json::Value, AccessError> {
    let parsed = parse(command)?;
    let canonical = canonical_command(&parsed);

    let mut connection = Connection::new();
    let mut transaction = connection
        .begin()
        .await
        .map_err(|e| error::from_statement(&e))?;
    match run(&mut transaction, command, &parsed, &canonical).await {
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

async fn run(
    transaction: &mut Transaction,
    command: &BookCommand,
    parsed: &Parsed,
    canonical: &[u8],
) -> Result<serde_json::Value, AccessError> {
    if let Some(replay) = sql::find_replay(transaction, command.idempotency_key.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
    {
        if replay.canonical_command != canonical {
            // Same key, different command. That is two bookings wearing one
            // identity, and answering either would be wrong.
            return Err(AccessError::field(
                AccessErrorKind::IdempotencyConflict,
                "value.idempotency_key",
            ));
        }
        let Some(status) = replay.status else {
            return Err(AccessError::retry());
        };
        return Ok(result(&replay.appointment_id.0, &status));
    }

    let claim = sql::claim_command(
        transaction,
        command.idempotency_key.clone(),
        canonical.to_vec(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    .ok_or_else(AccessError::retry)?;

    // THE SERIALIZATION POINT.
    sql::lock_dock(transaction, parsed.dock_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(|| {
            AccessError::missing(
                AccessErrorKind::DockNotFound,
                "value.dock_id",
                &parsed.dock_id.0,
            )
        })?;

    sql::load_carrier(transaction, parsed.carrier_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(|| {
            AccessError::missing(
                AccessErrorKind::CarrierNotFound,
                "value.carrier_id",
                &parsed.carrier_id.0,
            )
        })?;

    if let Some(taken) = sql::find_overlap(
        transaction,
        parsed.dock_id.clone(),
        parsed.slot_start.clone(),
        parsed.slot_end.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    {
        // The refusal names the appointment already holding the slot, so the
        // caller can ask about it rather than guess which booking blocked it.
        return Err(AccessError::missing(
            AccessErrorKind::SlotUnavailable,
            "value.slot_start",
            &taken.id.0,
        ));
    }

    let booked = sql::insert_appointment(
        transaction,
        claim.appointment_id.clone(),
        parsed.carrier_id.clone(),
        parsed.dock_id.clone(),
        parsed.slot_start.clone(),
        parsed.slot_end.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;

    let finalized = sql::finalize_command(
        transaction,
        command.idempotency_key.clone(),
        canonical.to_vec(),
        claim.appointment_id.clone(),
        booked.status.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    if finalized.status.is_none() {
        return Err(AccessError::retry());
    }

    Ok(result(&claim.appointment_id.0, &booked.status))
}

fn result(appointment_id: &str, status: &str) -> serde_json::Value {
    serde_json::json!({
        "appointment_id": appointment_id,
        "status": status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(slot_start: &str, slot_end: &str) -> BookCommand {
        BookCommand {
            idempotency_key: "k".to_owned(),
            carrier_id: "00000000-0000-0000-0000-000000000101".to_owned(),
            dock_id: "00000000-0000-0000-0000-000000000201".to_owned(),
            slot_start: slot_start.to_owned(),
            slot_end: slot_end.to_owned(),
        }
    }

    /// Two spellings of one booking are ONE command under the key: the
    /// canonical bytes come from the respelled scalars, not the caller's. The
    /// uuid half is already refused at the input port, whose released pattern
    /// pins a lowercase-hyphenated uuid; the OFFSET half reaches this code,
    /// because the port only asks the slot times for `format: date-time`.
    #[test]
    fn the_canonical_command_is_spelling_independent_and_excludes_the_key() {
        let offset = command("2026-10-01T11:00:00+02:00", "2026-10-01T12:00:00+02:00");
        let utc = command("2026-10-01T09:00:00Z", "2026-10-01T10:00:00.000000Z");
        let mut other_key = command("2026-10-01T09:00:00Z", "2026-10-01T10:00:00Z");
        other_key.idempotency_key = "different".to_owned();
        let bytes = |command: &BookCommand| canonical_command(&parse(command).unwrap());
        assert_eq!(bytes(&offset), bytes(&utc));
        assert_eq!(bytes(&utc), bytes(&other_key));
        assert!(
            !String::from_utf8(bytes(&utc))
                .unwrap()
                .contains("idempotency_key")
        );
    }

    /// A booking that differs in WHEN it is is a different command, so the
    /// bytes must still separate one from another.
    #[test]
    fn a_different_slot_is_a_different_command() {
        let morning = command("2026-10-01T09:00:00Z", "2026-10-01T10:00:00Z");
        let noon = command("2026-10-01T12:00:00Z", "2026-10-01T13:00:00Z");
        let bytes = |command: &BookCommand| canonical_command(&parse(command).unwrap());
        assert_ne!(bytes(&morning), bytes(&noon));
    }

    #[test]
    fn an_unspellable_scalar_is_refused_before_any_statement() {
        let mut broken = command("yesterday", "2026-10-01T10:00:00Z");
        assert_eq!(
            parse(&broken).unwrap_err().detail()["field"],
            "value.slot_start"
        );
        broken = command("2026-10-01T09:00:00Z", "2026-10-01T10:00:00Z");
        broken.dock_id = "not-a-uuid".to_owned();
        assert_eq!(
            parse(&broken).unwrap_err().detail()["field"],
            "value.dock_id"
        );
    }

    /// A slot that ends before it starts, or lasts no time at all, is not a
    /// slot. Refusing here names the field; the table's check constraint could
    /// only report `internal_error`.
    #[test]
    fn an_empty_or_reversed_slot_refuses() {
        for (start, end) in [
            ("2026-10-01T10:00:00Z", "2026-10-01T09:00:00Z"),
            ("2026-10-01T10:00:00Z", "2026-10-01T10:00:00Z"),
        ] {
            let error = parse(&command(start, end)).unwrap_err();
            assert_eq!(error.kind(), AccessErrorKind::InvalidInput);
            assert_eq!(error.detail()["field"], "value.slot_end");
        }
    }
}

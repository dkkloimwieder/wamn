//! `appointment.check_in` -- record the arrival against a booked appointment.
//!
//! Status only moves forward, so the transition is guarded twice: the
//! appointment row is locked and read, and the update itself carries
//! `status = 'scheduled'` in its WHERE. A repeat under a NEW key therefore
//! refuses rather than overwriting the arrival the first check-in recorded,
//! and a repeat under the SAME key replays the original answer.

use serde::Deserialize;
use wamn_postgres_statements::{Connection, TimestampTz, Transaction, Uuid};

use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::appointment_check_in as sql;
use crate::scalar;

pub(crate) const REFUSALS: &[AccessErrorKind] = &[
    AccessErrorKind::InvalidInput,
    AccessErrorKind::NotFound,
    AccessErrorKind::AppointmentNotScheduled,
    AccessErrorKind::IdempotencyConflict,
    AccessErrorKind::Retry,
    AccessErrorKind::Timeout,
    AccessErrorKind::PermissionDenied,
    AccessErrorKind::InternalError,
];

/// One envelope item's command body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckInCommand {
    pub(crate) idempotency_key: String,
    pub(crate) appointment_id: String,
    pub(crate) arrived_at: String,
}

/// The command's scalars in their one wire spelling.
#[derive(Debug)]
struct Parsed {
    appointment_id: Uuid,
    arrived_at: TimestampTz,
}

fn parse(command: &CheckInCommand) -> Result<Parsed, AccessError> {
    Ok(Parsed {
        appointment_id: scalar::uuid("appointment_id", &command.appointment_id)?,
        arrived_at: scalar::timestamp("arrived_at", &command.arrived_at)?,
    })
}

/// The bytes the idempotency key keys: the RE-SPELLED command, less the key.
fn canonical_command(parsed: &Parsed) -> Vec<u8> {
    wamn_execution_contract::canonical_json_bytes(&serde_json::json!({
        "appointment_id": parsed.appointment_id.0,
        "arrived_at": parsed.arrived_at.0,
    }))
}

/// # Errors
///
/// [`AccessError`] carrying the literal and detail the operation contract
/// declares for that refusal.
pub(crate) async fn execute(command: &CheckInCommand) -> Result<serde_json::Value, AccessError> {
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
    command: &CheckInCommand,
    parsed: &Parsed,
    canonical: &[u8],
) -> Result<serde_json::Value, AccessError> {
    if let Some(replay) = sql::find_replay(transaction, command.idempotency_key.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
    {
        if replay.canonical_command != canonical {
            return Err(AccessError::field(
                AccessErrorKind::IdempotencyConflict,
                "idempotency_key",
            ));
        }
        let (Some(status), Some(arrived_at)) = (replay.status, replay.arrived_at) else {
            return Err(AccessError::retry());
        };
        return Ok(result(
            &replay.check_in_id.0,
            &parsed.appointment_id.0,
            &status,
            &arrived_at.0,
        ));
    }

    let claim = sql::claim_command(
        transaction,
        command.idempotency_key.clone(),
        canonical.to_vec(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    .ok_or_else(AccessError::retry)?;

    let locked = sql::lock_appointment(transaction, parsed.appointment_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(|| {
            AccessError::missing(
                AccessErrorKind::NotFound,
                "appointment_id",
                &parsed.appointment_id.0,
            )
        })?;
    if locked.status != scalar::SCHEDULED {
        return Err(AccessError::missing(
            AccessErrorKind::AppointmentNotScheduled,
            "appointment_id",
            &parsed.appointment_id.0,
        ));
    }

    let arrived = sql::record_arrival(
        transaction,
        parsed.appointment_id.clone(),
        parsed.arrived_at.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?
    // The lock above already proved the row is scheduled, so the guarded
    // update matching nothing means the row moved under a lock this
    // transaction holds, which cannot happen. Refusing beats reporting a
    // result this transaction did not write.
    .ok_or_else(AccessError::retry)?;

    let recorded = arrived.arrived_at.clone();
    let finalized = sql::finalize_command(
        transaction,
        command.idempotency_key.clone(),
        canonical.to_vec(),
        claim.check_in_id.clone(),
        arrived.status.clone(),
        recorded.clone(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;
    if finalized.status.is_none() {
        return Err(AccessError::retry());
    }

    Ok(result(
        &claim.check_in_id.0,
        &arrived.id.0,
        &arrived.status,
        &recorded.0,
    ))
}

fn result(
    check_in_id: &str,
    appointment_id: &str,
    status: &str,
    arrived_at: &str,
) -> serde_json::Value {
    serde_json::json!({
        "check_in_id": check_in_id,
        "appointment_id": appointment_id,
        "status": status,
        "arrived_at": arrived_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(arrived_at: &str) -> CheckInCommand {
        CheckInCommand {
            idempotency_key: "k".to_owned(),
            appointment_id: "00000000-0000-0000-0000-000000000301".to_owned(),
            arrived_at: arrived_at.to_owned(),
        }
    }

    #[test]
    fn the_canonical_command_is_spelling_independent_and_excludes_the_key() {
        let offset = command("2026-10-01T11:07:00+02:00");
        let utc = command("2026-10-01T09:07:00Z");
        let mut other_key = command("2026-10-01T09:07:00.000000Z");
        other_key.idempotency_key = "different".to_owned();
        let bytes = |command: &CheckInCommand| canonical_command(&parse(command).unwrap());
        assert_eq!(bytes(&offset), bytes(&utc));
        assert_eq!(bytes(&utc), bytes(&other_key));
        assert_ne!(bytes(&utc), bytes(&command("2026-10-01T09:08:00Z")));
    }

    #[test]
    fn an_unspellable_scalar_is_refused_before_any_statement() {
        let mut broken = command("noon");
        assert_eq!(
            parse(&broken).unwrap_err().detail()["field"],
            "arrived_at"
        );
        broken = command("2026-10-01T09:07:00Z");
        broken.appointment_id = "not-a-uuid".to_owned();
        assert_eq!(
            parse(&broken).unwrap_err().detail()["field"],
            "appointment_id"
        );
    }

    #[test]
    fn the_result_carries_the_recorded_arrival_and_the_new_status() {
        let value = result("c", "a", "arrived", "2026-10-01T09:07:00.000000Z");
        assert_eq!(value["status"], "arrived");
        assert_eq!(value["arrived_at"], "2026-10-01T09:07:00.000000Z");
        assert_eq!(value["appointment_id"], "a");
    }
}

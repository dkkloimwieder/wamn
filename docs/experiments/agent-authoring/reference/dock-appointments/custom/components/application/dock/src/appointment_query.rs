//! `appointment.query` -- one dock's appointments for one day.
//!
//! Dispatch asks for a dock, a day and a status, and gets the answer in slot
//! order. The day boundary is decided in [`crate::scalar::day`] and bound as
//! two timestamps, so the session time zone never decides which day a slot
//! belongs to.

use serde::Deserialize;
use serde_json::{Value, json};
use wamn_postgres_statements::Connection;

use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::appointment_query as sql;
use crate::scalar;

pub(crate) const REFUSALS: &[AccessErrorKind] = &[
    AccessErrorKind::InvalidInput,
    AccessErrorKind::DockNotFound,
    AccessErrorKind::Retry,
    AccessErrorKind::Timeout,
    AccessErrorKind::PermissionDenied,
    AccessErrorKind::InternalError,
];

/// One envelope item's read body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QueryInput {
    pub(crate) dock_id: String,
    pub(crate) day: String,
    pub(crate) status: String,
}

/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub(crate) async fn execute(
    connection: &mut Connection,
    input: &QueryInput,
) -> Result<Value, AccessError> {
    let dock_id = scalar::uuid("dock_id", &input.dock_id)?;
    let (day_start, day_end) = scalar::day("day", &input.day)?;
    let status = scalar::status("status", &input.status)?;

    let mut transaction = connection
        .begin()
        .await
        .map_err(|e| error::from_statement(&e))?;
    let answered = read(
        &mut transaction,
        &dock_id,
        &day_start,
        &day_end,
        &status,
        input,
    )
    .await;
    match answered {
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

async fn read(
    transaction: &mut wamn_postgres_statements::Transaction,
    dock_id: &wamn_postgres_statements::Uuid,
    day_start: &wamn_postgres_statements::TimestampTz,
    day_end: &wamn_postgres_statements::TimestampTz,
    status: &str,
    input: &QueryInput,
) -> Result<Value, AccessError> {
    // An unknown dock is not an empty day. Saying so lets dispatch tell a
    // quiet dock from a mistyped one.
    sql::load_dock(transaction, dock_id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(|| {
            AccessError::missing(AccessErrorKind::DockNotFound, "dock_id", &dock_id.0)
        })?;

    let rows = sql::appointment_query(
        transaction,
        dock_id.clone(),
        day_start.clone(),
        day_end.clone(),
        status.to_owned(),
    )
    .await
    .map_err(|e| error::from_statement(&e))?;

    Ok(json!({
        "dock_id": dock_id.0,
        "day": input.day,
        "status": status,
        "appointments": rows.iter().map(row_to_json).collect::<Vec<_>>(),
    }))
}

fn row_to_json(row: &sql::AppointmentQueryRow) -> Value {
    json!({
        "appointment_id": row.id.0,
        "carrier_id": row.carrier_id.0,
        "dock_id": row.dock_id.0,
        "slot_start": row.slot_start.0,
        "slot_end": row.slot_end.0,
        "status": row.status,
        "arrived_at": row.arrived_at.as_ref().map(|value| value.0.clone()),
    })
}

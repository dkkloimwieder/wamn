// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ClaimCommandRow {
    pub check_in_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FinalizeCommandRow {
    pub status: Option<String>,
    pub arrived_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub check_in_id: uuid::Uuid,
    pub status: Option<String>,
    pub arrived_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct LockAppointmentRow {
    pub id: uuid::Uuid,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct RecordArrivalRow {
    pub id: uuid::Uuid,
    pub status: String,
    pub arrived_at: chrono::DateTime<chrono::Utc>,
}

pub(crate) const CLAIM_COMMAND_SQL: &str = include_str!("../../command/appointment_check_in/claim_command.sql");
pub(crate) const FINALIZE_COMMAND_SQL: &str = include_str!("../../command/appointment_check_in/finalize_command.sql");
pub(crate) const FIND_REPLAY_SQL: &str = include_str!("../../command/appointment_check_in/find_replay.sql");
pub(crate) const LOCK_APPOINTMENT_SQL: &str = include_str!("../../command/appointment_check_in/lock_appointment.sql");
pub(crate) const RECORD_ARRIVAL_SQL: &str = include_str!("../../command/appointment_check_in/record_arrival.sql");

pub(crate) fn claim_command_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn claim_command_canonical_command_bind_fixture() -> Vec<u8> {
    Vec::new()
}
pub(crate) fn finalize_command_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn finalize_command_canonical_command_bind_fixture() -> Vec<u8> {
    Vec::new()
}
pub(crate) fn finalize_command_check_in_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn finalize_command_status_bind_fixture() -> String {
    String::new()
}
pub(crate) fn finalize_command_arrived_at_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn find_replay_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn lock_appointment_appointment_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn record_arrival_appointment_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn record_arrival_arrived_at_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}

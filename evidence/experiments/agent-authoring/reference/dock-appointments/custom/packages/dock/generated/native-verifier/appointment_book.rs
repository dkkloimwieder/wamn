// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ClaimCommandRow {
    pub appointment_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FinalizeCommandRow {
    pub status: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FindOverlapRow {
    pub id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub appointment_id: uuid::Uuid,
    pub status: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct InsertAppointmentRow {
    pub id: uuid::Uuid,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct LoadCarrierRow {
    pub id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct LockDockRow {
    pub id: uuid::Uuid,
}

pub(crate) const CLAIM_COMMAND_SQL: &str = include_str!("../../command/appointment_book/claim_command.sql");
pub(crate) const FINALIZE_COMMAND_SQL: &str = include_str!("../../command/appointment_book/finalize_command.sql");
pub(crate) const FIND_OVERLAP_SQL: &str = include_str!("../../command/appointment_book/find_overlap.sql");
pub(crate) const FIND_REPLAY_SQL: &str = include_str!("../../command/appointment_book/find_replay.sql");
pub(crate) const INSERT_APPOINTMENT_SQL: &str = include_str!("../../command/appointment_book/insert_appointment.sql");
pub(crate) const LOAD_CARRIER_SQL: &str = include_str!("../../command/appointment_book/load_carrier.sql");
pub(crate) const LOCK_DOCK_SQL: &str = include_str!("../../command/appointment_book/lock_dock.sql");

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
pub(crate) fn finalize_command_appointment_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn finalize_command_status_bind_fixture() -> String {
    String::new()
}
pub(crate) fn find_overlap_dock_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn find_overlap_slot_start_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn find_overlap_slot_end_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn find_replay_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn insert_appointment_appointment_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_appointment_carrier_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_appointment_dock_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_appointment_slot_start_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn insert_appointment_slot_end_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn load_carrier_carrier_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn lock_dock_dock_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

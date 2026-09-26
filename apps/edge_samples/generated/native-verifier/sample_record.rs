// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct ClaimSampleRow {
    pub sample_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct FindSampleRow {
    pub canonical_command: Vec<u8>,
    pub sample_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct RecordSampleRow {
    pub id: uuid::Uuid,
}

pub(crate) const CLAIM_SAMPLE_SQL: &str = include_str!("../../command/sample/claim.sql");
pub(crate) const FIND_SAMPLE_SQL: &str = include_str!("../../command/sample/replay.sql");
pub(crate) const RECORD_SAMPLE_SQL: &str = include_str!("../../command/sample/record.sql");

pub(crate) fn claim_sample_canonical_command_bind_fixture() -> Vec<u8> {
    Vec::new()
}
pub(crate) fn claim_sample_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn find_sample_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn record_sample_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn record_sample_frame_bind_fixture() -> String {
    String::new()
}
pub(crate) fn record_sample_captured_at_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}

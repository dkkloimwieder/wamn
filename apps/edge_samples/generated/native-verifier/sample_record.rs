// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct RecordSampleRow {
    pub id: uuid::Uuid,
}

pub(crate) const RECORD_SAMPLE_SQL: &str = include_str!("../../command/sample/record.sql");

pub(crate) fn record_sample_frame_bind_fixture() -> String {
    String::new()
}
pub(crate) fn record_sample_captured_at_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}

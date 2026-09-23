// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct ClaimBatchRow {
    pub widget_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct FinalizeBatchRow {
    pub widget_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct FindBatchRow {
    pub canonical_command: Vec<u8>,
    pub widget_id: uuid::Uuid,
}

pub(crate) const CLAIM_BATCH_SQL: &str = include_str!("../../command/widget/claim.sql");
pub(crate) const FINALIZE_BATCH_SQL: &str = include_str!("../../command/widget/finalize.sql");
pub(crate) const FIND_BATCH_SQL: &str = include_str!("../../command/widget/replay.sql");

pub(crate) fn claim_batch_canonical_command_bind_fixture() -> Vec<u8> {
    Vec::new()
}
pub(crate) fn claim_batch_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn finalize_batch_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn find_batch_idempotency_key_bind_fixture() -> String {
    String::new()
}

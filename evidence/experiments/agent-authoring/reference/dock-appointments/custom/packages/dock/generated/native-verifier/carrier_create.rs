// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ClaimCommandRow {
    pub carrier_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FinalizeCommandRow {
    pub finalized: Option<bool>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub carrier_id: uuid::Uuid,
    pub finalized: Option<bool>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct InsertCarrierRow {
    pub id: uuid::Uuid,
}

pub(crate) const CLAIM_COMMAND_SQL: &str = include_str!("../../command/carrier_create/claim_command.sql");
pub(crate) const FINALIZE_COMMAND_SQL: &str = include_str!("../../command/carrier_create/finalize_command.sql");
pub(crate) const FIND_REPLAY_SQL: &str = include_str!("../../command/carrier_create/find_replay.sql");
pub(crate) const INSERT_CARRIER_SQL: &str = include_str!("../../command/carrier_create/insert_carrier.sql");

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
pub(crate) fn finalize_command_carrier_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn find_replay_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn insert_carrier_carrier_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_carrier_name_bind_fixture() -> String {
    String::new()
}

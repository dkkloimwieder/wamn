// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ClaimCommandRow {
    pub dock_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FinalizeCommandRow {
    pub finalized: Option<bool>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub dock_id: uuid::Uuid,
    pub finalized: Option<bool>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct InsertDockRow {
    pub id: uuid::Uuid,
}

pub(crate) const CLAIM_COMMAND_SQL: &str = include_str!("../../command/dock_create/claim_command.sql");
pub(crate) const FINALIZE_COMMAND_SQL: &str = include_str!("../../command/dock_create/finalize_command.sql");
pub(crate) const FIND_REPLAY_SQL: &str = include_str!("../../command/dock_create/find_replay.sql");
pub(crate) const INSERT_DOCK_SQL: &str = include_str!("../../command/dock_create/insert_dock.sql");

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
pub(crate) fn finalize_command_dock_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn find_replay_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn insert_dock_dock_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_dock_name_bind_fixture() -> String {
    String::new()
}

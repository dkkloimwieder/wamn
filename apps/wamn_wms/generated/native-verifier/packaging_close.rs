// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct ApplyRow {
    pub id: uuid::Uuid,
    pub r#type: String,
    pub code: String,
    pub location_id: uuid::Uuid,
    pub lifecycle: String,
    pub row_version: i32,
}

#[derive(Debug, sqlx::FromRow)]
pub struct ClaimCommandRow {
    pub operation_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct FinalizeCommandRow {
    pub result: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub result: Option<String>,
    pub operation_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct LockPackagingRow {
    pub id: uuid::Uuid,
    pub r#type: String,
    pub code: String,
    pub location_id: uuid::Uuid,
    pub lifecycle: String,
    pub row_version: i32,
}

#[derive(Debug, sqlx::FromRow)]
pub struct OpenInventoryRow {
    pub id: uuid::Uuid,
}

pub(crate) const APPLY_SQL: &str = include_str!("../../command/packaging_close/apply.sql");
pub(crate) const CLAIM_COMMAND_SQL: &str =
    include_str!("../../command/packaging_close/claim_command.sql");
pub(crate) const FINALIZE_COMMAND_SQL: &str =
    include_str!("../../command/packaging_close/finalize_command.sql");
pub(crate) const FIND_REPLAY_SQL: &str =
    include_str!("../../command/packaging_close/find_replay.sql");
pub(crate) const LOCK_PACKAGING_SQL: &str =
    include_str!("../../command/packaging_close/lock_packaging.sql");
pub(crate) const OPEN_INVENTORY_SQL: &str =
    include_str!("../../command/packaging_close/open_inventory.sql");

pub(crate) fn apply_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
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
pub(crate) fn finalize_command_operation_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn finalize_command_result_bind_fixture() -> String {
    String::new()
}
pub(crate) fn find_replay_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn lock_packaging_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn open_inventory_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

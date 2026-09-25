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
    pub packaging_id: uuid::Uuid,
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
    pub packaging_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct ValidateLocationRow {
    pub id: uuid::Uuid,
}

pub(crate) const APPLY_SQL: &str = include_str!("../../command/packaging_create/apply.sql");
pub(crate) const CLAIM_COMMAND_SQL: &str =
    include_str!("../../command/packaging_create/claim_command.sql");
pub(crate) const FINALIZE_COMMAND_SQL: &str =
    include_str!("../../command/packaging_create/finalize_command.sql");
pub(crate) const FIND_REPLAY_SQL: &str =
    include_str!("../../command/packaging_create/find_replay.sql");
pub(crate) const VALIDATE_LOCATION_SQL: &str =
    include_str!("../../command/packaging_create/validate_location.sql");

pub(crate) fn apply_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn apply_type_bind_fixture() -> String {
    String::new()
}
pub(crate) fn apply_code_bind_fixture() -> String {
    String::new()
}
pub(crate) fn apply_location_id_bind_fixture() -> uuid::Uuid {
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
pub(crate) fn validate_location_location_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

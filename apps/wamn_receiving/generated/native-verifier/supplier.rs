// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct SupplierRow {
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub id: uuid::Uuid,
    pub name: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct SupplierCreateClaimRow {
    pub supplier_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct SupplierCreateReplayRow {
    pub canonical_command: Vec<u8>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub id: uuid::Uuid,
    pub name: String,
}

pub(crate) const CREATE_0_SQL: &str = include_str!("../sql/supplier/create_claim.sql");
pub(crate) const CREATE_1_SQL: &str = include_str!("../sql/supplier/create_replay.sql");
pub(crate) const CREATE_2_SQL: &str = include_str!("../sql/supplier/create.sql");
pub(crate) const QUERY_SQL: &str = include_str!("../sql/supplier/query_created_at_ascending.sql");

pub(crate) fn query_created_at_ascending_cursor_key_bind_fixture()
-> Option<chrono::DateTime<chrono::Utc>> {
    None
}
pub(crate) fn query_created_at_ascending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_created_at_ascending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn create_claim_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn create_claim_canonical_command_bind_fixture() -> Vec<u8> {
    Vec::new()
}
pub(crate) fn create_replay_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn create_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn create_name_bind_fixture() -> String {
    String::new()
}

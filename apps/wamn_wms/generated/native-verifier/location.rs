// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct LocationRow {
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub id: uuid::Uuid,
    pub location_code: String,
    pub row_version: i32,
}

#[derive(Debug, sqlx::FromRow)]
pub struct LocationCreateClaimRow {
    pub location_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct LocationCreateReplayRow {
    pub canonical_command: Vec<u8>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub id: uuid::Uuid,
    pub location_code: String,
    pub row_version: i32,
}

#[derive(Debug, sqlx::FromRow)]
pub struct LocationUpdateRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i32>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub id: Option<uuid::Uuid>,
    pub location_code: Option<String>,
    pub row_version: Option<i32>,
}

pub(crate) const CREATE_0_SQL: &str = include_str!("../sql/location/create_claim.sql");
pub(crate) const CREATE_1_SQL: &str = include_str!("../sql/location/create_replay.sql");
pub(crate) const CREATE_2_SQL: &str = include_str!("../sql/location/create.sql");
pub(crate) const GET_SQL: &str = include_str!("../sql/location/get.sql");
pub(crate) const QUERY_SQL: &str = include_str!("../sql/location/query_created_at_ascending.sql");
pub(crate) const UPDATE_SQL: &str = include_str!("../sql/location/update.sql");

pub(crate) fn get_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn query_created_at_ascending_location_code_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
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
pub(crate) fn create_location_code_bind_fixture() -> String {
    String::new()
}
pub(crate) fn update_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn update_expected_row_version_bind_fixture() -> i32 {
    0_i32
}
pub(crate) fn update_location_code_present_bind_fixture() -> bool {
    false
}
pub(crate) fn update_location_code_value_bind_fixture() -> Option<String> {
    None
}

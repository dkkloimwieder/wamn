// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct PalletRow {
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub id: uuid::Uuid,
    pub location_id: uuid::Uuid,
    pub pallet_code: String,
    pub row_version: i64,
    pub status: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub(crate) const GET_SQL: &str = include_str!("../sql/pallet/get.sql");
pub(crate) const QUERY_0_SQL: &str = include_str!("../../query/open_pallet_by_pallet_code_ascending.sql");
pub(crate) const QUERY_1_SQL: &str = include_str!("../../query/open_pallet_by_pallet_code_descending.sql");
pub(crate) const QUERY_2_SQL: &str = include_str!("../../query/open_pallet_by_location_id_ascending.sql");
pub(crate) const QUERY_3_SQL: &str = include_str!("../../query/open_pallet_by_location_id_descending.sql");
pub(crate) const QUERY_4_SQL: &str = include_str!("../../query/open_pallet_by_updated_at_ascending.sql");
pub(crate) const QUERY_5_SQL: &str = include_str!("../../query/open_pallet_by_updated_at_descending.sql");
pub(crate) const QUERY_6_SQL: &str = include_str!("../../query/open_pallet.sql");
pub(crate) const QUERY_7_SQL: &str = include_str!("../../query/open_pallet_by_created_at_descending.sql");

pub(crate) fn get_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn query_pallet_code_ascending_status_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_pallet_code_ascending_location_id_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_pallet_code_ascending_pallet_code_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_pallet_code_ascending_cursor_key_bind_fixture() -> Option<String> {
    None
}
pub(crate) fn query_pallet_code_ascending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_pallet_code_ascending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn query_pallet_code_descending_status_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_pallet_code_descending_location_id_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_pallet_code_descending_pallet_code_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_pallet_code_descending_cursor_key_bind_fixture() -> Option<String> {
    None
}
pub(crate) fn query_pallet_code_descending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_pallet_code_descending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn query_location_id_ascending_status_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_location_id_ascending_location_id_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_location_id_ascending_pallet_code_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_location_id_ascending_cursor_key_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_location_id_ascending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_location_id_ascending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn query_location_id_descending_status_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_location_id_descending_location_id_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_location_id_descending_pallet_code_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_location_id_descending_cursor_key_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_location_id_descending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_location_id_descending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn query_updated_at_ascending_status_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_updated_at_ascending_location_id_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_updated_at_ascending_pallet_code_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_updated_at_ascending_cursor_key_bind_fixture() -> Option<chrono::DateTime<chrono::Utc>> {
    None
}
pub(crate) fn query_updated_at_ascending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_updated_at_ascending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn query_updated_at_descending_status_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_updated_at_descending_location_id_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_updated_at_descending_pallet_code_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_updated_at_descending_cursor_key_bind_fixture() -> Option<chrono::DateTime<chrono::Utc>> {
    None
}
pub(crate) fn query_updated_at_descending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_updated_at_descending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn query_created_at_ascending_status_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_created_at_ascending_location_id_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_created_at_ascending_pallet_code_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_created_at_ascending_cursor_key_bind_fixture() -> Option<chrono::DateTime<chrono::Utc>> {
    None
}
pub(crate) fn query_created_at_ascending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_created_at_ascending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn query_created_at_descending_status_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_created_at_descending_location_id_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_created_at_descending_pallet_code_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_created_at_descending_cursor_key_bind_fixture() -> Option<chrono::DateTime<chrono::Utc>> {
    None
}
pub(crate) fn query_created_at_descending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_created_at_descending_limit_bind_fixture() -> i64 {
    0_i64
}

// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct PackagingRow {
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub created_by: uuid::Uuid,
    pub id: uuid::Uuid,
    pub located_at: chrono::DateTime<chrono::Utc>,
    pub location_id: uuid::Uuid,
    pub packaging_code: String,
    pub row_version: i32,
    pub status: String,
    pub r#type: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub updated_by: uuid::Uuid,
}

pub(crate) const CREATE_SQL: &str = include_str!("../sql/packaging/create.sql");
pub(crate) const GET_SQL: &str = include_str!("../sql/packaging/get.sql");
pub(crate) const QUERY_0_SQL: &str =
    include_str!("../../query/open_packaging_by_packaging_code_ascending.sql");
pub(crate) const QUERY_1_SQL: &str =
    include_str!("../../query/open_packaging_by_packaging_code_descending.sql");
pub(crate) const QUERY_2_SQL: &str =
    include_str!("../../query/open_packaging_by_location_id_ascending.sql");
pub(crate) const QUERY_3_SQL: &str =
    include_str!("../../query/open_packaging_by_location_id_descending.sql");
pub(crate) const QUERY_4_SQL: &str =
    include_str!("../../query/open_packaging_by_updated_at_ascending.sql");
pub(crate) const QUERY_5_SQL: &str =
    include_str!("../../query/open_packaging_by_updated_at_descending.sql");
pub(crate) const QUERY_6_SQL: &str = include_str!("../../query/open_packaging.sql");
pub(crate) const QUERY_7_SQL: &str =
    include_str!("../../query/open_packaging_by_created_at_descending.sql");

pub(crate) fn get_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn query_packaging_code_ascending_status_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
pub(crate) fn query_packaging_code_ascending_location_id_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
pub(crate) fn query_packaging_code_ascending_packaging_code_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
pub(crate) fn query_packaging_code_ascending_cursor_key_bind_fixture() -> Option<String> {
    None
}
pub(crate) fn query_packaging_code_ascending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_packaging_code_ascending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn query_packaging_code_descending_status_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
pub(crate) fn query_packaging_code_descending_location_id_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
pub(crate) fn query_packaging_code_descending_packaging_code_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
pub(crate) fn query_packaging_code_descending_cursor_key_bind_fixture() -> Option<String> {
    None
}
pub(crate) fn query_packaging_code_descending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_packaging_code_descending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn query_location_id_ascending_status_filter_bind_fixture() -> Option<serde_json::Value>
{
    None
}
pub(crate) fn query_location_id_ascending_location_id_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
pub(crate) fn query_location_id_ascending_packaging_code_filter_bind_fixture()
-> Option<serde_json::Value> {
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
pub(crate) fn query_location_id_descending_status_filter_bind_fixture() -> Option<serde_json::Value>
{
    None
}
pub(crate) fn query_location_id_descending_location_id_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
pub(crate) fn query_location_id_descending_packaging_code_filter_bind_fixture()
-> Option<serde_json::Value> {
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
pub(crate) fn query_updated_at_ascending_location_id_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
pub(crate) fn query_updated_at_ascending_packaging_code_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
pub(crate) fn query_updated_at_ascending_cursor_key_bind_fixture()
-> Option<chrono::DateTime<chrono::Utc>> {
    None
}
pub(crate) fn query_updated_at_ascending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_updated_at_ascending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn query_updated_at_descending_status_filter_bind_fixture() -> Option<serde_json::Value>
{
    None
}
pub(crate) fn query_updated_at_descending_location_id_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
pub(crate) fn query_updated_at_descending_packaging_code_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
pub(crate) fn query_updated_at_descending_cursor_key_bind_fixture()
-> Option<chrono::DateTime<chrono::Utc>> {
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
pub(crate) fn query_created_at_ascending_location_id_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
pub(crate) fn query_created_at_ascending_packaging_code_filter_bind_fixture()
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
pub(crate) fn query_created_at_descending_status_filter_bind_fixture() -> Option<serde_json::Value>
{
    None
}
pub(crate) fn query_created_at_descending_location_id_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
pub(crate) fn query_created_at_descending_packaging_code_filter_bind_fixture()
-> Option<serde_json::Value> {
    None
}
pub(crate) fn query_created_at_descending_cursor_key_bind_fixture()
-> Option<chrono::DateTime<chrono::Utc>> {
    None
}
pub(crate) fn query_created_at_descending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_created_at_descending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn create_packaging_code_bind_fixture() -> String {
    String::new()
}
pub(crate) fn create_type_bind_fixture() -> String {
    String::new()
}
pub(crate) fn create_location_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn create_status_bind_fixture() -> String {
    String::new()
}

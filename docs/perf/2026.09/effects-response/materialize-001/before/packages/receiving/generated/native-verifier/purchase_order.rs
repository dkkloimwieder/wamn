// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct PurchaseOrderRow {
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub id: uuid::Uuid,
    pub purchase_order_number: String,
    pub row_version: i64,
    pub status: String,
    pub supplier_id: uuid::Uuid,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct PurchaseOrderUpdateRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i64>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub id: Option<uuid::Uuid>,
    pub purchase_order_number: Option<String>,
    pub row_version: Option<i64>,
    pub status: Option<String>,
    pub supplier_id: Option<uuid::Uuid>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub(crate) const GET_SQL: &str = include_str!("../sql/purchase_order/get.sql");
pub(crate) const QUERY_0_SQL: &str = include_str!("../../query/open_purchase_order_by_purchase_order_number_ascending.sql");
pub(crate) const QUERY_1_SQL: &str = include_str!("../../query/open_purchase_order_by_purchase_order_number_descending.sql");
pub(crate) const QUERY_2_SQL: &str = include_str!("../../query/open_purchase_order_by_status_ascending.sql");
pub(crate) const QUERY_3_SQL: &str = include_str!("../../query/open_purchase_order_by_status_descending.sql");
pub(crate) const QUERY_4_SQL: &str = include_str!("../../query/open_purchase_order.sql");
pub(crate) const QUERY_5_SQL: &str = include_str!("../../query/open_purchase_order_by_created_at_descending.sql");
pub(crate) const UPDATE_SQL: &str = include_str!("../sql/purchase_order/update.sql");

pub(crate) fn get_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn query_purchase_order_number_ascending_supplier_id_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_purchase_order_number_ascending_status_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_purchase_order_number_ascending_cursor_key_bind_fixture() -> Option<String> {
    None
}
pub(crate) fn query_purchase_order_number_ascending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_purchase_order_number_ascending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn query_purchase_order_number_descending_supplier_id_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_purchase_order_number_descending_status_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_purchase_order_number_descending_cursor_key_bind_fixture() -> Option<String> {
    None
}
pub(crate) fn query_purchase_order_number_descending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_purchase_order_number_descending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn query_status_ascending_supplier_id_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_status_ascending_status_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_status_ascending_cursor_key_bind_fixture() -> Option<String> {
    None
}
pub(crate) fn query_status_ascending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_status_ascending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn query_status_descending_supplier_id_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_status_descending_status_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_status_descending_cursor_key_bind_fixture() -> Option<String> {
    None
}
pub(crate) fn query_status_descending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_status_descending_limit_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn query_created_at_ascending_supplier_id_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_created_at_ascending_status_filter_bind_fixture() -> Option<serde_json::Value> {
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
pub(crate) fn query_created_at_descending_supplier_id_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_created_at_descending_status_filter_bind_fixture() -> Option<serde_json::Value> {
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
pub(crate) fn update_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn update_expected_row_version_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn update_supplier_id_present_bind_fixture() -> bool {
    false
}
pub(crate) fn update_supplier_id_value_bind_fixture() -> Option<uuid::Uuid> {
    None
}

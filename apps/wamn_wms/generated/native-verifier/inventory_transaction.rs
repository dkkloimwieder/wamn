// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct InventoryTransactionRow {
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub from_disposition: Option<String>,
    pub from_inventory_id: uuid::Uuid,
    pub from_lifecycle: Option<String>,
    pub from_location_id: Option<uuid::Uuid>,
    pub from_packaging_id: Option<uuid::Uuid>,
    pub from_product_id: Option<uuid::Uuid>,
    pub from_quantity: rust_decimal::Decimal,
    pub id: uuid::Uuid,
    pub inventory_id: uuid::Uuid,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub operation_id: uuid::Uuid,
    pub reason: Option<String>,
    pub to_disposition: String,
    pub to_inventory_id: uuid::Uuid,
    pub to_lifecycle: String,
    pub to_location_id: uuid::Uuid,
    pub to_packaging_id: uuid::Uuid,
    pub to_product_id: uuid::Uuid,
    pub to_quantity: rust_decimal::Decimal,
    pub r#type: String,
}

pub(crate) const GET_SQL: &str = include_str!("../sql/inventory_transaction/get.sql");
pub(crate) const QUERY_SQL: &str =
    include_str!("../sql/inventory_transaction/query_created_at_ascending.sql");

pub(crate) fn get_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
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

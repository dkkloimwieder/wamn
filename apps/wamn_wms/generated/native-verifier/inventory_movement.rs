// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct InventoryMovementRow {
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub created_by: uuid::Uuid,
    pub from_location_id: Option<uuid::Uuid>,
    pub id: uuid::Uuid,
    pub idempotency_key: String,
    pub kind: String,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub pallet_id: uuid::Uuid,
    pub product_id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
    pub reason_code: Option<String>,
    pub to_location_id: Option<uuid::Uuid>,
}

pub(crate) const GET_SQL: &str = include_str!("../sql/inventory_movement/get.sql");
pub(crate) const QUERY_SQL: &str =
    include_str!("../sql/inventory_movement/query_created_at_ascending.sql");

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

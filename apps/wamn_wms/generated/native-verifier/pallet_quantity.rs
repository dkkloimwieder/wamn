// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct PalletQuantityRow {
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub id: uuid::Uuid,
    pub pallet_id: uuid::Uuid,
    pub product_id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
    pub status: String,
}

pub(crate) const GET_SQL: &str = include_str!("../sql/pallet_quantity/get.sql");
pub(crate) const QUERY_SQL: &str =
    include_str!("../sql/pallet_quantity/query_created_at_ascending.sql");

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

// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct InsertMovementRow {
    pub id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct LockPalletRow {
    pub location_id: uuid::Uuid,
    pub row_version: i32,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct SetQuantityRow {
    pub id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
}

#[derive(Debug, sqlx::FromRow)]
pub struct TouchPalletRow {
    pub row_version: i32,
    pub status: String,
}

pub(crate) const INSERT_MOVEMENT_SQL: &str =
    include_str!("../../command/inventory_adjust/insert_movement.sql");
pub(crate) const LOCK_PALLET_SQL: &str =
    include_str!("../../command/inventory_adjust/lock_pallet.sql");
pub(crate) const SET_QUANTITY_SQL: &str =
    include_str!("../../command/inventory_adjust/set_quantity.sql");
pub(crate) const TOUCH_PALLET_SQL: &str =
    include_str!("../../command/inventory_adjust/touch_pallet.sql");

pub(crate) fn insert_movement_pallet_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_movement_product_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_movement_quantity_bind_fixture() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ZERO
}
pub(crate) fn insert_movement_reason_code_bind_fixture() -> String {
    String::new()
}
pub(crate) fn insert_movement_occurred_at_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn lock_pallet_pallet_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn set_quantity_pallet_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn set_quantity_product_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn set_quantity_status_bind_fixture() -> String {
    String::new()
}
pub(crate) fn set_quantity_quantity_bind_fixture() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ZERO
}
pub(crate) fn touch_pallet_pallet_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

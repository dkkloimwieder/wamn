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
pub struct MovePalletRow {
    pub location_id: uuid::Uuid,
    pub row_version: i32,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct SelectPalletQuantityRow {
    pub product_id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct ValidateLocationRow {
    pub id: uuid::Uuid,
}

pub(crate) const INSERT_MOVEMENT_SQL: &str =
    include_str!("../../command/inventory_move/insert_movement.sql");
pub(crate) const LOCK_PALLET_SQL: &str =
    include_str!("../../command/inventory_move/lock_pallet.sql");
pub(crate) const MOVE_PALLET_SQL: &str =
    include_str!("../../command/inventory_move/move_pallet.sql");
pub(crate) const SELECT_PALLET_QUANTITY_SQL: &str =
    include_str!("../../command/inventory_move/select_pallet_quantity.sql");
pub(crate) const VALIDATE_LOCATION_SQL: &str =
    include_str!("../../command/inventory_move/validate_location.sql");

pub(crate) fn insert_movement_pallet_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_movement_product_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_movement_from_location_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_movement_to_location_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_movement_quantity_bind_fixture() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ZERO
}
pub(crate) fn insert_movement_occurred_at_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn lock_pallet_pallet_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn move_pallet_pallet_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn move_pallet_to_location_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn select_pallet_quantity_pallet_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn validate_location_location_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

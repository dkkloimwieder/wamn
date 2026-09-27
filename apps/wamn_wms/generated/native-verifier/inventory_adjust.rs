// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct DeleteQuantityRow {
    pub id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct InsertTransactionRow {
    pub id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct LockPackagingRow {
    pub location_id: uuid::Uuid,
    pub row_version: i32,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct SelectQuantityRow {
    pub quantity: rust_decimal::Decimal,
}

#[derive(Debug, sqlx::FromRow)]
pub struct SetQuantityRow {
    pub id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
}

#[derive(Debug, sqlx::FromRow)]
pub struct TouchPackagingRow {
    pub row_version: i32,
    pub status: String,
}

pub(crate) const DELETE_QUANTITY_SQL: &str =
    include_str!("../../command/inventory_adjust/delete_quantity.sql");
pub(crate) const INSERT_TRANSACTION_SQL: &str =
    include_str!("../../command/inventory_adjust/insert_transaction.sql");
pub(crate) const LOCK_PACKAGING_SQL: &str =
    include_str!("../../command/inventory_adjust/lock_packaging.sql");
pub(crate) const SELECT_QUANTITY_SQL: &str =
    include_str!("../../command/inventory_adjust/select_quantity.sql");
pub(crate) const SET_QUANTITY_SQL: &str =
    include_str!("../../command/inventory_adjust/set_quantity.sql");
pub(crate) const TOUCH_PACKAGING_SQL: &str =
    include_str!("../../command/inventory_adjust/touch_packaging.sql");

pub(crate) fn delete_quantity_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn delete_quantity_product_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn delete_quantity_status_bind_fixture() -> String {
    String::new()
}
pub(crate) fn insert_transaction_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_transaction_product_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_transaction_status_bind_fixture() -> String {
    String::new()
}
pub(crate) fn insert_transaction_quantity_bind_fixture() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ZERO
}
pub(crate) fn insert_transaction_reason_code_bind_fixture() -> String {
    String::new()
}
pub(crate) fn insert_transaction_occurred_at_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn lock_packaging_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn select_quantity_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn select_quantity_product_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn select_quantity_status_bind_fixture() -> String {
    String::new()
}
pub(crate) fn set_quantity_packaging_id_bind_fixture() -> uuid::Uuid {
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
pub(crate) fn touch_packaging_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

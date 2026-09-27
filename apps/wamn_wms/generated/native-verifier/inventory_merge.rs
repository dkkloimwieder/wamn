// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct AddToTargetRow {
    pub id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
}

#[derive(Debug, sqlx::FromRow)]
pub struct ConsumeSourceRow {
    pub row_version: i32,
}

#[derive(Debug, sqlx::FromRow)]
pub struct DeleteSourceQuantityRow {
    pub id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct InsertTransactionRow {
    pub id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct LockBothPackagingsRow {
    pub id: uuid::Uuid,
    pub location_id: uuid::Uuid,
    pub row_version: i32,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct PlaceOnTargetRow {
    pub id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
}

#[derive(Debug, sqlx::FromRow)]
pub struct SelectSourceQuantityRow {
    pub product_id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct TouchTargetRow {
    pub row_version: i32,
    pub status: String,
}

pub(crate) const ADD_TO_TARGET_SQL: &str =
    include_str!("../../command/inventory_merge/add_to_target.sql");
pub(crate) const CONSUME_SOURCE_SQL: &str =
    include_str!("../../command/inventory_merge/consume_source.sql");
pub(crate) const DELETE_SOURCE_QUANTITY_SQL: &str =
    include_str!("../../command/inventory_merge/delete_source_quantity.sql");
pub(crate) const INSERT_TRANSACTION_SQL: &str =
    include_str!("../../command/inventory_merge/insert_transaction.sql");
pub(crate) const LOCK_BOTH_PACKAGINGS_SQL: &str =
    include_str!("../../command/inventory_merge/lock_both_packagings.sql");
pub(crate) const PLACE_ON_TARGET_SQL: &str =
    include_str!("../../command/inventory_merge/place_on_target.sql");
pub(crate) const SELECT_SOURCE_QUANTITY_SQL: &str =
    include_str!("../../command/inventory_merge/select_source_quantity.sql");
pub(crate) const TOUCH_TARGET_SQL: &str =
    include_str!("../../command/inventory_merge/touch_target.sql");

pub(crate) fn add_to_target_target_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn add_to_target_product_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn add_to_target_status_bind_fixture() -> String {
    String::new()
}
pub(crate) fn add_to_target_quantity_bind_fixture() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ZERO
}
pub(crate) fn consume_source_source_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn delete_source_quantity_source_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_transaction_product_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_transaction_quantity_bind_fixture() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ZERO
}
pub(crate) fn insert_transaction_source_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_transaction_status_bind_fixture() -> String {
    String::new()
}
pub(crate) fn insert_transaction_target_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_transaction_occurred_at_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn lock_both_packagings_source_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn lock_both_packagings_target_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn place_on_target_target_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn place_on_target_product_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn place_on_target_status_bind_fixture() -> String {
    String::new()
}
pub(crate) fn place_on_target_quantity_bind_fixture() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ZERO
}
pub(crate) fn select_source_quantity_source_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn touch_target_target_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

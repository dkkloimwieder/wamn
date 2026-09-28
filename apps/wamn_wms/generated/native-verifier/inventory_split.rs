// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct CreatePackagingRow {
    pub id: uuid::Uuid,
    pub row_version: i32,
    pub status: String,
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
pub struct PlaceQuantityRow {
    pub id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
}

#[derive(Debug, sqlx::FromRow)]
pub struct SelectQuantityRow {
    pub quantity: rust_decimal::Decimal,
}

#[derive(Debug, sqlx::FromRow)]
pub struct TakeFromSourceRow {
    pub id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
}

#[derive(Debug, sqlx::FromRow)]
pub struct TouchSourceRow {
    pub row_version: i32,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct ValidateLocationRow {
    pub id: uuid::Uuid,
}

pub(crate) const CREATE_PACKAGING_SQL: &str =
    include_str!("../../command/inventory_split/create_packaging.sql");
pub(crate) const INSERT_TRANSACTION_SQL: &str =
    include_str!("../../command/inventory_split/insert_transaction.sql");
pub(crate) const LOCK_PACKAGING_SQL: &str =
    include_str!("../../command/inventory_split/lock_packaging.sql");
pub(crate) const PLACE_QUANTITY_SQL: &str =
    include_str!("../../command/inventory_split/place_quantity.sql");
pub(crate) const SELECT_QUANTITY_SQL: &str =
    include_str!("../../command/inventory_split/select_quantity.sql");
pub(crate) const TAKE_FROM_SOURCE_SQL: &str =
    include_str!("../../command/inventory_split/take_from_source.sql");
pub(crate) const TOUCH_SOURCE_SQL: &str =
    include_str!("../../command/inventory_split/touch_source.sql");
pub(crate) const VALIDATE_LOCATION_SQL: &str =
    include_str!("../../command/inventory_split/validate_location.sql");

pub(crate) fn create_packaging_new_packaging_code_bind_fixture() -> String {
    String::new()
}
pub(crate) fn create_packaging_new_packaging_type_bind_fixture() -> String {
    String::new()
}
pub(crate) fn create_packaging_to_location_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn create_packaging_occurred_at_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn create_packaging_status_bind_fixture() -> String {
    String::new()
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
pub(crate) fn insert_transaction_new_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_transaction_occurred_at_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn lock_packaging_source_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn place_quantity_new_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn place_quantity_product_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn place_quantity_status_bind_fixture() -> String {
    String::new()
}
pub(crate) fn place_quantity_quantity_bind_fixture() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ZERO
}
pub(crate) fn select_quantity_source_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn select_quantity_product_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn select_quantity_status_bind_fixture() -> String {
    String::new()
}
pub(crate) fn take_from_source_source_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn take_from_source_product_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn take_from_source_status_bind_fixture() -> String {
    String::new()
}
pub(crate) fn take_from_source_quantity_bind_fixture() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ZERO
}
pub(crate) fn touch_source_source_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn validate_location_to_location_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

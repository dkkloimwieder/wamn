// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct ApplyRow {
    pub id: uuid::Uuid,
    pub product_id: uuid::Uuid,
    pub packaging_id: uuid::Uuid,
    pub location_id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
    pub disposition: String,
    pub lifecycle: String,
    pub row_version: i32,
}

#[derive(Debug, sqlx::FromRow)]
pub struct ClaimCommandRow {
    pub operation_id: uuid::Uuid,
    pub new_inventory_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct CreateInventoryRow {
    pub id: uuid::Uuid,
    pub product_id: uuid::Uuid,
    pub packaging_id: uuid::Uuid,
    pub location_id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
    pub disposition: String,
    pub lifecycle: String,
    pub row_version: i32,
}

#[derive(Debug, sqlx::FromRow)]
pub struct FinalizeCommandRow {
    pub result: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub result: Option<String>,
    pub operation_id: uuid::Uuid,
    pub new_inventory_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct InsertTransactionRow {
    pub id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct LockInventoryRow {
    pub id: uuid::Uuid,
    pub product_id: uuid::Uuid,
    pub packaging_id: uuid::Uuid,
    pub location_id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
    pub disposition: String,
    pub lifecycle: String,
    pub row_version: i32,
}

#[derive(Debug, sqlx::FromRow)]
pub struct LockPackagingRow {
    pub id: uuid::Uuid,
    pub r#type: String,
    pub code: String,
    pub location_id: uuid::Uuid,
    pub lifecycle: String,
    pub row_version: i32,
}

pub(crate) const APPLY_SQL: &str = include_str!("../../command/inventory_split/apply.sql");
pub(crate) const CLAIM_COMMAND_SQL: &str =
    include_str!("../../command/inventory_split/claim_command.sql");
pub(crate) const CREATE_INVENTORY_SQL: &str =
    include_str!("../../command/inventory_split/create_inventory.sql");
pub(crate) const FINALIZE_COMMAND_SQL: &str =
    include_str!("../../command/inventory_split/finalize_command.sql");
pub(crate) const FIND_REPLAY_SQL: &str =
    include_str!("../../command/inventory_split/find_replay.sql");
pub(crate) const INSERT_TRANSACTION_SQL: &str =
    include_str!("../../command/inventory_split/insert_transaction.sql");
pub(crate) const LOCK_INVENTORY_SQL: &str =
    include_str!("../../command/inventory_split/lock_inventory.sql");
pub(crate) const LOCK_PACKAGING_SQL: &str =
    include_str!("../../command/inventory_split/lock_packaging.sql");

pub(crate) fn apply_inventory_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn apply_quantity_bind_fixture() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ZERO
}
pub(crate) fn claim_command_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn claim_command_canonical_command_bind_fixture() -> Vec<u8> {
    Vec::new()
}
pub(crate) fn create_inventory_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn create_inventory_product_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn create_inventory_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn create_inventory_location_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn create_inventory_quantity_bind_fixture() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ZERO
}
pub(crate) fn create_inventory_disposition_bind_fixture() -> String {
    String::new()
}
pub(crate) fn create_inventory_lifecycle_bind_fixture() -> String {
    String::new()
}
pub(crate) fn finalize_command_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn finalize_command_canonical_command_bind_fixture() -> Vec<u8> {
    Vec::new()
}
pub(crate) fn finalize_command_operation_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn finalize_command_result_bind_fixture() -> String {
    String::new()
}
pub(crate) fn find_replay_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn insert_transaction_operation_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_transaction_inventory_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_transaction_from_inventory_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_transaction_to_inventory_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_transaction_from_product_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn insert_transaction_from_packaging_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn insert_transaction_from_location_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn insert_transaction_from_quantity_bind_fixture() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ZERO
}
pub(crate) fn insert_transaction_from_disposition_bind_fixture() -> Option<String> {
    None
}
pub(crate) fn insert_transaction_from_lifecycle_bind_fixture() -> Option<String> {
    None
}
pub(crate) fn insert_transaction_occurred_at_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn insert_transaction_reason_bind_fixture() -> Option<String> {
    None
}
pub(crate) fn insert_transaction_to_product_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_transaction_to_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_transaction_to_location_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_transaction_to_quantity_bind_fixture() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ZERO
}
pub(crate) fn insert_transaction_to_disposition_bind_fixture() -> String {
    String::new()
}
pub(crate) fn insert_transaction_to_lifecycle_bind_fixture() -> String {
    String::new()
}
pub(crate) fn lock_inventory_inventory_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn lock_packaging_from_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn lock_packaging_to_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

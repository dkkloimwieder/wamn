// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ClaimCommandRow {
    pub movement_id: uuid::Uuid,
    pub new_pallet_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct CreatePalletRow {
    pub id: uuid::Uuid,
    pub row_version: i64,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FinalizeCommandRow {
    pub row_version: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub movement_id: uuid::Uuid,
    pub source_pallet_id: uuid::Uuid,
    pub new_pallet_id: uuid::Uuid,
    pub row_version: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct InsertMovementRow {
    pub id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct LockPalletRow {
    pub location_id: uuid::Uuid,
    pub row_version: i64,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct PlaceQuantityRow {
    pub id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct TakeFromSourceRow {
    pub id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct TouchSourceRow {
    pub row_version: i64,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ValidateLocationRow {
    pub id: uuid::Uuid,
}

pub(crate) const CLAIM_COMMAND_SQL: &str = include_str!("../../command/inventory_split/claim_command.sql");
pub(crate) const CREATE_PALLET_SQL: &str = include_str!("../../command/inventory_split/create_pallet.sql");
pub(crate) const FINALIZE_COMMAND_SQL: &str = include_str!("../../command/inventory_split/finalize_command.sql");
pub(crate) const FIND_REPLAY_SQL: &str = include_str!("../../command/inventory_split/find_replay.sql");
pub(crate) const INSERT_MOVEMENT_SQL: &str = include_str!("../../command/inventory_split/insert_movement.sql");
pub(crate) const LOCK_PALLET_SQL: &str = include_str!("../../command/inventory_split/lock_pallet.sql");
pub(crate) const PLACE_QUANTITY_SQL: &str = include_str!("../../command/inventory_split/place_quantity.sql");
pub(crate) const TAKE_FROM_SOURCE_SQL: &str = include_str!("../../command/inventory_split/take_from_source.sql");
pub(crate) const TOUCH_SOURCE_SQL: &str = include_str!("../../command/inventory_split/touch_source.sql");
pub(crate) const VALIDATE_LOCATION_SQL: &str = include_str!("../../command/inventory_split/validate_location.sql");

pub(crate) fn claim_command_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn claim_command_canonical_command_bind_fixture() -> Vec<u8> {
    Vec::new()
}
pub(crate) fn claim_command_source_pallet_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn create_pallet_new_pallet_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn create_pallet_new_pallet_code_bind_fixture() -> String {
    String::new()
}
pub(crate) fn create_pallet_to_location_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn create_pallet_status_bind_fixture() -> String {
    String::new()
}
pub(crate) fn finalize_command_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn finalize_command_canonical_command_bind_fixture() -> Vec<u8> {
    Vec::new()
}
pub(crate) fn finalize_command_movement_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn finalize_command_row_version_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn find_replay_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn insert_movement_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn insert_movement_pallet_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_movement_product_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_movement_quantity_bind_fixture() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ZERO
}
pub(crate) fn insert_movement_occurred_at_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn lock_pallet_source_pallet_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn place_quantity_new_pallet_id_bind_fixture() -> uuid::Uuid {
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
pub(crate) fn take_from_source_source_pallet_id_bind_fixture() -> uuid::Uuid {
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
pub(crate) fn touch_source_source_pallet_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn validate_location_to_location_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

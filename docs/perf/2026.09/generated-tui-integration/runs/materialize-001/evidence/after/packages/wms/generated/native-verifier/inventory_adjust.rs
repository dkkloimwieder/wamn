// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ClaimCommandRow {
    pub movement_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FinalizeCommandRow {
    pub adjusted_quantity: Option<rust_decimal::Decimal>,
    pub row_version: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub movement_id: uuid::Uuid,
    pub pallet_id: uuid::Uuid,
    pub adjusted_quantity: Option<rust_decimal::Decimal>,
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
pub(crate) struct SetQuantityRow {
    pub id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct TouchPalletRow {
    pub row_version: i64,
    pub status: String,
}

pub(crate) const CLAIM_COMMAND_SQL: &str = include_str!("../../command/inventory_adjust/claim_command.sql");
pub(crate) const FINALIZE_COMMAND_SQL: &str = include_str!("../../command/inventory_adjust/finalize_command.sql");
pub(crate) const FIND_REPLAY_SQL: &str = include_str!("../../command/inventory_adjust/find_replay.sql");
pub(crate) const INSERT_MOVEMENT_SQL: &str = include_str!("../../command/inventory_adjust/insert_movement.sql");
pub(crate) const LOCK_PALLET_SQL: &str = include_str!("../../command/inventory_adjust/lock_pallet.sql");
pub(crate) const SET_QUANTITY_SQL: &str = include_str!("../../command/inventory_adjust/set_quantity.sql");
pub(crate) const TOUCH_PALLET_SQL: &str = include_str!("../../command/inventory_adjust/touch_pallet.sql");

pub(crate) fn claim_command_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn claim_command_canonical_command_bind_fixture() -> Vec<u8> {
    Vec::new()
}
pub(crate) fn claim_command_pallet_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
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
pub(crate) fn finalize_command_adjusted_quantity_bind_fixture() -> rust_decimal::Decimal {
    rust_decimal::Decimal::ZERO
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

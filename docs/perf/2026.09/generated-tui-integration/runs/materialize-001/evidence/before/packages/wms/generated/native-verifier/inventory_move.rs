// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ClaimCommandRow {
    pub movement_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FinalizeCommandRow {
    pub pallet_status: Option<String>,
    pub row_version: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub movement_id: uuid::Uuid,
    pub pallet_id: uuid::Uuid,
    pub pallet_status: Option<String>,
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
pub(crate) struct MovePalletRow {
    pub location_id: uuid::Uuid,
    pub row_version: i64,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct SelectPalletQuantityRow {
    pub product_id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ValidateLocationRow {
    pub id: uuid::Uuid,
}

pub(crate) const CLAIM_COMMAND_SQL: &str = include_str!("../../command/inventory_move/claim_command.sql");
pub(crate) const FINALIZE_COMMAND_SQL: &str = include_str!("../../command/inventory_move/finalize_command.sql");
pub(crate) const FIND_REPLAY_SQL: &str = include_str!("../../command/inventory_move/find_replay.sql");
pub(crate) const INSERT_MOVEMENT_SQL: &str = include_str!("../../command/inventory_move/insert_movement.sql");
pub(crate) const LOCK_PALLET_SQL: &str = include_str!("../../command/inventory_move/lock_pallet.sql");
pub(crate) const MOVE_PALLET_SQL: &str = include_str!("../../command/inventory_move/move_pallet.sql");
pub(crate) const SELECT_PALLET_QUANTITY_SQL: &str = include_str!("../../command/inventory_move/select_pallet_quantity.sql");
pub(crate) const VALIDATE_LOCATION_SQL: &str = include_str!("../../command/inventory_move/validate_location.sql");

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
pub(crate) fn finalize_command_pallet_status_bind_fixture() -> String {
    String::new()
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

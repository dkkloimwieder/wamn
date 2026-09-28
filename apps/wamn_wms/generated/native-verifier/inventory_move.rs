// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct LockPackagingRow {
    pub location_id: uuid::Uuid,
    pub row_version: i32,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct MovePackagingRow {
    pub location_id: uuid::Uuid,
    pub row_version: i32,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct ValidateLocationRow {
    pub id: uuid::Uuid,
}

pub(crate) const LOCK_PACKAGING_SQL: &str =
    include_str!("../../command/inventory_move/lock_packaging.sql");
pub(crate) const MOVE_PACKAGING_SQL: &str =
    include_str!("../../command/inventory_move/move_packaging.sql");
pub(crate) const VALIDATE_LOCATION_SQL: &str =
    include_str!("../../command/inventory_move/validate_location.sql");

pub(crate) fn lock_packaging_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn move_packaging_packaging_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn move_packaging_to_location_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn move_packaging_occurred_at_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn validate_location_location_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

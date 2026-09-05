// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct PalletRow {
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub id: uuid::Uuid,
    pub location_id: uuid::Uuid,
    pub pallet_code: String,
    pub row_version: i64,
    pub status: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

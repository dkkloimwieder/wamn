// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct QualityInspectionRow {
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub created_by: uuid::Uuid,
    pub receipt_id: uuid::Uuid,
    pub row_version: i64,
    pub status: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub updated_by: uuid::Uuid,
}

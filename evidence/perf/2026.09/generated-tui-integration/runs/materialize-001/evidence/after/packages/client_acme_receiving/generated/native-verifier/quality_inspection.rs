// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct QualityInspectionRow {
    pub receipt_id: uuid::Uuid,
    pub row_version: i64,
    pub status: String,
}

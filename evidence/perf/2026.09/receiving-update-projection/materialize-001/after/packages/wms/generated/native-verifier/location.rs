// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct LocationRow {
    pub id: uuid::Uuid,
    pub location_code: String,
}

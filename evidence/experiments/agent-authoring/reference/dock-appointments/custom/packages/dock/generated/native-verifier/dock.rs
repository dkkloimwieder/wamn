// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct DockRow {
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub id: uuid::Uuid,
    pub name: String,
}

// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct WidgetRow {
    pub code: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub edit_version: i64,
    pub id: uuid::Uuid,
    pub maker_id: Option<uuid::Uuid>,
    pub note: Option<String>,
    pub overlay_note: Option<String>,
}

pub(crate) const GET_SQL: &str = include_str!("../sql/widget/get.sql");

pub(crate) fn get_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct SampleRow {
    pub captured_at: chrono::DateTime<chrono::Utc>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub frame: String,
    pub id: uuid::Uuid,
}

pub(crate) const GET_SQL: &str = include_str!("../sql/sample/get.sql");

pub(crate) fn get_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

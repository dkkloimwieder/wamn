// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct PackagingRow {
    pub code: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub id: uuid::Uuid,
    pub lifecycle: String,
    pub location_id: uuid::Uuid,
    pub row_version: i32,
    pub r#type: String,
}

pub(crate) const GET_SQL: &str = include_str!("../sql/packaging/get.sql");
pub(crate) const QUERY_SQL: &str = include_str!("../sql/packaging/query_created_at_ascending.sql");

pub(crate) fn get_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn query_created_at_ascending_cursor_key_bind_fixture()
-> Option<chrono::DateTime<chrono::Utc>> {
    None
}
pub(crate) fn query_created_at_ascending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_created_at_ascending_limit_bind_fixture() -> i64 {
    0_i64
}

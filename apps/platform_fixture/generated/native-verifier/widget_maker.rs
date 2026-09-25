// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct WidgetMakerRow {
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub id: uuid::Uuid,
    pub name: String,
}

pub(crate) const GET_SQL: &str = include_str!("../sql/widget_maker/get.sql");
pub(crate) const QUERY_0_SQL: &str =
    include_str!("../sql/widget_maker/query_created_at_ascending.sql");
pub(crate) const QUERY_1_SQL: &str =
    include_str!("../sql/widget_maker/query_created_at_descending.sql");

pub(crate) fn get_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn query_created_at_ascending_name_filter_bind_fixture() -> Option<serde_json::Value> {
    None
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
pub(crate) fn query_created_at_descending_name_filter_bind_fixture() -> Option<serde_json::Value> {
    None
}
pub(crate) fn query_created_at_descending_cursor_key_bind_fixture()
-> Option<chrono::DateTime<chrono::Utc>> {
    None
}
pub(crate) fn query_created_at_descending_cursor_id_bind_fixture() -> Option<uuid::Uuid> {
    None
}
pub(crate) fn query_created_at_descending_limit_bind_fixture() -> i64 {
    0_i64
}

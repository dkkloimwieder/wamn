// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct WidgetTagRow {
    pub edit_version: i64,
    pub id: uuid::Uuid,
    pub label: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct WidgetTagUpdateRow {
    pub outcome: Option<String>,
    pub observed_edit_version: Option<i64>,
    pub edit_version: Option<i64>,
    pub id: Option<uuid::Uuid>,
    pub label: Option<String>,
}

pub(crate) const UPDATE_SQL: &str = include_str!("../sql/widget_tag/update.sql");

pub(crate) fn update_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn update_expected_edit_version_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn update_label_present_bind_fixture() -> bool {
    false
}
pub(crate) fn update_label_value_bind_fixture() -> Option<String> {
    None
}

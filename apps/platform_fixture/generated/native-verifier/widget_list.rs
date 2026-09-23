// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct ListRow {
    pub id: uuid::Uuid,
    pub code: String,
    pub edit_version: i64,
}

pub(crate) const LIST_SQL: &str = include_str!("../../query/widget_list.sql");

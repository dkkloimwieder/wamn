// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct ListRow {
    pub id: uuid::Uuid,
    pub name: String,
}

pub(crate) const LIST_SQL: &str = include_str!("../../query/widget_maker_list.sql");

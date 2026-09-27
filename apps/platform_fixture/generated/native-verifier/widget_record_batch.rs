// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct FindWidgetRow {
    pub widget_id: uuid::Uuid,
}

pub(crate) const FIND_WIDGET_SQL: &str = include_str!("../../command/widget/record_batch.sql");

pub(crate) fn find_widget_widget_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

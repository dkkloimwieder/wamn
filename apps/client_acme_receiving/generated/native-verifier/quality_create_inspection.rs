// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct InsertInspectionRow {
    pub receipt_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct LoadInspectionRow {
    pub receipt_id: uuid::Uuid,
}

pub(crate) const INSERT_INSPECTION_SQL: &str = include_str!("../../command/create_inspection/insert_inspection.sql");
pub(crate) const LOAD_INSPECTION_SQL: &str = include_str!("../../command/create_inspection/load_inspection.sql");

pub(crate) fn insert_inspection_receipt_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn load_inspection_receipt_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

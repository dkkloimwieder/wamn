// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ApproveInspectionRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i64>,
    pub receipt_id: Option<uuid::Uuid>,
    pub status: Option<String>,
    pub row_version: Option<i64>,
    pub purchase_order_id: Option<uuid::Uuid>,
    pub purchase_order_row_version: Option<i64>,
}

pub(crate) const APPROVE_INSPECTION_SQL: &str = include_str!("../../command/approve_inspection/approve_inspection.sql");

pub(crate) fn approve_inspection_receipt_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn approve_inspection_expected_row_version_bind_fixture() -> i64 {
    0_i64
}

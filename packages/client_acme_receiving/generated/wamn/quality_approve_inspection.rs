// @generated from migration IR; do not edit.

use sqlx_core::query_as::query_as;
use sqlx_core::transaction::Transaction;
use wamn_postgres_sqlx::WamnPostgres;

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ApproveInspectionRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i64>,
    pub receipt_id: Option<wamn_postgres_sqlx::Uuid>,
    pub status: Option<String>,
    pub row_version: Option<i64>,
    pub purchase_order_id: Option<wamn_postgres_sqlx::Uuid>,
    pub purchase_order_row_version: Option<i64>,
}

pub(crate) const APPROVE_INSPECTION_SQL: &str = include_str!("../../command/approve_inspection/approve_inspection.sql");

pub(crate) async fn approve_inspection(
    transaction: &mut Transaction<'_, WamnPostgres>,
    receipt_id: wamn_postgres_sqlx::Uuid,
    expected_row_version: i64,
) -> Result<ApproveInspectionRow, sqlx_core::error::Error> {
    query_as::<WamnPostgres, ApproveInspectionRow>(APPROVE_INSPECTION_SQL)
        .bind(receipt_id)
        .bind(expected_row_version)
        .fetch_one(&mut **transaction)
        .await
}

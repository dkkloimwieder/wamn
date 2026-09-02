// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct ApproveInspectionRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i64>,
    pub receipt_id: Option<wamn_postgres_statements::Uuid>,
    pub status: Option<String>,
    pub row_version: Option<i64>,
    pub purchase_order_id: Option<wamn_postgres_statements::Uuid>,
    pub purchase_order_row_version: Option<i64>,
}

pub(crate) const APPROVE_INSPECTION_DIGEST: &str = "sha256:1049669b64fe0491694988be7e38771a8c4068839c06371b2bd3a81cd09140c0";

pub(crate) async fn approve_inspection(
    transaction: &mut Transaction,
    receipt_id: wamn_postgres_statements::Uuid,
    expected_row_version: i64,
) -> Result<ApproveInspectionRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(APPROVE_INSPECTION_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(receipt_id),
        wamn_postgres_statements::into_sql_value(expected_row_version),
    ]).await?;
    wamn_postgres_statements::decode_one(APPROVE_INSPECTION_DIGEST, rows, |row| {
        Ok(ApproveInspectionRow {
            outcome: row.decode("outcome")?,
            observed_row_version: row.decode("observed_row_version")?,
            receipt_id: row.decode("receipt_id")?,
            status: row.decode("status")?,
            row_version: row.decode("row_version")?,
            purchase_order_id: row.decode("purchase_order_id")?,
            purchase_order_row_version: row.decode("purchase_order_row_version")?,
        })
    })
}

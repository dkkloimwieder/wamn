// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct LoadPurchaseOrderDetailRow {
    pub id: wamn_postgres_statements::Uuid,
    pub purchase_order_number: String,
    pub supplier_id: wamn_postgres_statements::Uuid,
    pub status: String,
    pub row_version: i64,
    pub acme_inspection_required: bool,
    pub acme_quality_status: String,
}

pub(crate) const LOAD_PURCHASE_ORDER_DETAIL_DIGEST: &str = "sha256:80ee1d1fc0391b2167f4ad4360f2c8d2876d5d0303dc1996cb1a757288eb536f";

pub(crate) async fn load_purchase_order_detail(
    transaction: &mut Transaction,
    purchase_order_id: wamn_postgres_statements::Uuid,
) -> Result<Option<LoadPurchaseOrderDetailRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(LOAD_PURCHASE_ORDER_DETAIL_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(purchase_order_id),
    ]).await?;
    wamn_postgres_statements::decode_optional(LOAD_PURCHASE_ORDER_DETAIL_DIGEST, rows, |row| {
        Ok(LoadPurchaseOrderDetailRow {
            id: row.decode("id")?,
            purchase_order_number: row.decode("purchase_order_number")?,
            supplier_id: row.decode("supplier_id")?,
            status: row.decode("status")?,
            row_version: row.decode("row_version")?,
            acme_inspection_required: row.decode("acme_inspection_required")?,
            acme_quality_status: row.decode("acme_quality_status")?,
        })
    })
}

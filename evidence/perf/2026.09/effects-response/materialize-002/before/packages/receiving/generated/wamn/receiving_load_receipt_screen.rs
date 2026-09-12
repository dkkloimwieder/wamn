// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct LoadReceiptScreenRow {
    pub purchase_order_id: wamn_postgres_statements::Uuid,
    pub purchase_order_number: String,
    pub purchase_order_status: String,
    pub supplier_id: wamn_postgres_statements::Uuid,
    pub row_version: i64,
    pub line_id: Option<wamn_postgres_statements::Uuid>,
    pub line_number: Option<i32>,
    pub item_id: Option<wamn_postgres_statements::Uuid>,
    pub item_number: Option<String>,
    pub ordered_quantity: Option<wamn_postgres_statements::Numeric>,
    pub received_quantity: Option<wamn_postgres_statements::Numeric>,
    pub remaining_quantity: Option<wamn_postgres_statements::Numeric>,
}

pub(crate) const LOAD_RECEIPT_SCREEN_DIGEST: &str = "sha256:d10b99c537b6c8b826c9d8e4f61f3aae4683073aaefc0915356ae7b41157b068";

pub(crate) async fn load_receipt_screen(
    transaction: &mut Transaction,
    purchase_order_id: wamn_postgres_statements::Uuid,
) -> Result<Vec<LoadReceiptScreenRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(LOAD_RECEIPT_SCREEN_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(purchase_order_id),
    ]).await?;
    wamn_postgres_statements::decode_all(LOAD_RECEIPT_SCREEN_DIGEST, rows, |row| {
        Ok(LoadReceiptScreenRow {
            purchase_order_id: row.decode("purchase_order_id")?,
            purchase_order_number: row.decode("purchase_order_number")?,
            purchase_order_status: row.decode("purchase_order_status")?,
            supplier_id: row.decode("supplier_id")?,
            row_version: row.decode("row_version")?,
            line_id: row.decode("line_id")?,
            line_number: row.decode("line_number")?,
            item_id: row.decode("item_id")?,
            item_number: row.decode("item_number")?,
            ordered_quantity: row.decode("ordered_quantity")?,
            received_quantity: row.decode("received_quantity")?,
            remaining_quantity: row.decode("remaining_quantity")?,
        })
    })
}

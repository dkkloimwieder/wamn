// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct LoadPurchaseOrderHistoryRow {
    pub position: i64,
    pub kind: String,
    pub operation: String,
    pub changed_by: wamn_postgres_statements::Uuid,
    pub changed_at: wamn_postgres_statements::TimestampTz,
    pub transaction_id: i64,
    pub before: Option<String>,
    pub after: Option<String>,
    pub current: Option<String>,
    pub head_position: Option<i64>,
}

pub(crate) const LOAD_PURCHASE_ORDER_HISTORY_DIGEST: &str = "sha256:985bbbfdaaf76a1939b287a21c2c805c8cfaa8ea007d73c2199bb85d04f238fd";

pub(crate) async fn load_purchase_order_history(
    transaction: &mut Transaction,
    id: wamn_postgres_statements::Uuid,
    after_position: i64,
    limit: i64,
) -> Result<Vec<LoadPurchaseOrderHistoryRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(LOAD_PURCHASE_ORDER_HISTORY_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(id),
        wamn_postgres_statements::into_sql_value(after_position),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(LOAD_PURCHASE_ORDER_HISTORY_DIGEST, rows, |row| {
        Ok(LoadPurchaseOrderHistoryRow {
            position: row.decode("position")?,
            kind: row.decode("kind")?,
            operation: row.decode("operation")?,
            changed_by: row.decode("changed_by")?,
            changed_at: row.decode("changed_at")?,
            transaction_id: row.decode("transaction_id")?,
            before: row.decode("before")?,
            after: row.decode("after")?,
            current: row.decode("current")?,
            head_position: row.decode("head_position")?,
        })
    })
}

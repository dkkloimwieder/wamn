// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct LoadPurchaseOrderHistoryRow {
    pub id: wamn_postgres_statements::Uuid,
    pub position: i64,
    pub type_: String,
    pub operation: String,
    pub changed_by: wamn_postgres_statements::Uuid,
    pub changed_at: wamn_postgres_statements::TimestampTz,
    pub before: Option<String>,
    pub after: Option<String>,
    pub current: Option<String>,
}

pub(crate) const LOAD_PURCHASE_ORDER_HISTORY_DIGEST: &str =
    "sha256:4e66e64f790f4435bdd16964f4944355ecfb3af51b141289663fa8a673bba147";

pub(crate) async fn load_purchase_order_history(
    transaction: &mut Transaction,
    id: wamn_postgres_statements::Uuid,
    after_position: i64,
    limit: i32,
) -> Result<Vec<LoadPurchaseOrderHistoryRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            LOAD_PURCHASE_ORDER_HISTORY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(after_position),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(LOAD_PURCHASE_ORDER_HISTORY_DIGEST, rows, |row| {
        Ok(LoadPurchaseOrderHistoryRow {
            id: row.decode("id")?,
            position: row.decode("position")?,
            type_: row.decode("type")?,
            operation: row.decode("operation")?,
            changed_by: row.decode("changed_by")?,
            changed_at: row.decode("changed_at")?,
            before: row.decode("before")?,
            after: row.decode("after")?,
            current: row.decode("current")?,
        })
    })
}

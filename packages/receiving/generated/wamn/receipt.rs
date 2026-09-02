// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct ReceiptRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub id: wamn_postgres_statements::Uuid,
    pub idempotency_key: String,
    pub occurred_at: wamn_postgres_statements::TimestampTz,
    pub purchase_order_id: wamn_postgres_statements::Uuid,
    pub receipt_reference: String,
}

pub(crate) const GET_DIGEST: &str = "sha256:73e18a76c60b67894ed4b7abb362953da0cc6d29254054096454ae837e3245d1";
pub(crate) const QUERY_DIGEST: &str = "sha256:7c9d3439ce29d392049e5bda1e005e146eb4882d22fee290ed7de80c3b4e1eb3";

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<ReceiptRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(GET_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(id),
    ]).await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(ReceiptRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            idempotency_key: row.decode("idempotency_key")?,
            occurred_at: row.decode("occurred_at")?,
            purchase_order_id: row.decode("purchase_order_id")?,
            receipt_reference: row.decode("receipt_reference")?,
        })
    })
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<ReceiptRow>, wamn_postgres_statements::StatementError> {
    let rows = connection.run(QUERY_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(cursor_key),
        wamn_postgres_statements::into_sql_value(cursor_id),
        wamn_postgres_statements::into_sql_value(limit),
    ]).await?;
    wamn_postgres_statements::decode_all(QUERY_DIGEST, rows, |row| {
        Ok(ReceiptRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            idempotency_key: row.decode("idempotency_key")?,
            occurred_at: row.decode("occurred_at")?,
            purchase_order_id: row.decode("purchase_order_id")?,
            receipt_reference: row.decode("receipt_reference")?,
        })
    })
}

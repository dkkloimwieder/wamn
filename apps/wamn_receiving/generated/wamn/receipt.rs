// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct ReceiptRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub created_by: wamn_postgres_statements::Uuid,
    pub id: wamn_postgres_statements::Uuid,
    pub idempotency_key: String,
    pub occurred_at: wamn_postgres_statements::TimestampTz,
    pub purchase_order_id: wamn_postgres_statements::Uuid,
    pub receipt_reference: String,
}

pub(crate) const GET_DIGEST: &str =
    "sha256:0761529775d51c86b2f7a77c646630c0384012476c84d7dc99e027616cf3afd2";
pub(crate) const QUERY_DIGEST: &str =
    "sha256:2814a03759be697b6ad57b0c484db94dbf0ae628db238c4b32c1ab7cf2718706";

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<ReceiptRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(ReceiptRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
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
) -> Result<wamn_postgres_statements::RowStream<ReceiptRow>, wamn_postgres_statements::StatementError>
{
    connection
        .run_stream(
            QUERY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(ReceiptRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    id: row.decode("id")?,
                    idempotency_key: row.decode("idempotency_key")?,
                    occurred_at: row.decode("occurred_at")?,
                    purchase_order_id: row.decode("purchase_order_id")?,
                    receipt_reference: row.decode("receipt_reference")?,
                })
            },
        )
        .await
}

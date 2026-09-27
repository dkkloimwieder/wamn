// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct InventoryTransactionRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub created_by: wamn_postgres_statements::Uuid,
    pub from_packaging_id: Option<wamn_postgres_statements::Uuid>,
    pub from_status: Option<String>,
    pub id: wamn_postgres_statements::Uuid,
    pub occurred_at: wamn_postgres_statements::TimestampTz,
    pub product_id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
    pub reason_code: Option<String>,
    pub to_packaging_id: Option<wamn_postgres_statements::Uuid>,
    pub to_status: Option<String>,
}

pub(crate) const GET_DIGEST: &str =
    "sha256:7ac93007659ec7b784975776da1d7448d1bedabed7ab87acd14b6f0a7c560519";
pub(crate) const QUERY_DIGEST: &str =
    "sha256:5af65710047d9a01473cae0990325fc20050a7896eee6bf090c8c9ffad6e6675";

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<InventoryTransactionRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(InventoryTransactionRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            from_packaging_id: row.decode("from_packaging_id")?,
            from_status: row.decode("from_status")?,
            id: row.decode("id")?,
            occurred_at: row.decode("occurred_at")?,
            product_id: row.decode("product_id")?,
            quantity: row.decode("quantity")?,
            reason_code: row.decode("reason_code")?,
            to_packaging_id: row.decode("to_packaging_id")?,
            to_status: row.decode("to_status")?,
        })
    })
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<InventoryTransactionRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(InventoryTransactionRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    from_packaging_id: row.decode("from_packaging_id")?,
                    from_status: row.decode("from_status")?,
                    id: row.decode("id")?,
                    occurred_at: row.decode("occurred_at")?,
                    product_id: row.decode("product_id")?,
                    quantity: row.decode("quantity")?,
                    reason_code: row.decode("reason_code")?,
                    to_packaging_id: row.decode("to_packaging_id")?,
                    to_status: row.decode("to_status")?,
                })
            },
        )
        .await
}

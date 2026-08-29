// @generated from migration IR; do not edit.

use sqlx_core::query_as::query_as;
use wamn_postgres_sqlx::{WamnConnection, WamnPostgres};

#[derive(Debug, sqlx::FromRow)]
pub struct ReceiptRow {
    pub created_at: wamn_postgres_sqlx::TimestampTz,
    pub id: wamn_postgres_sqlx::Uuid,
    pub idempotency_key: String,
    pub occurred_at: wamn_postgres_sqlx::TimestampTz,
    pub purchase_order_id: wamn_postgres_sqlx::Uuid,
    pub receipt_reference: String,
}

pub(crate) const GET_SQL: &str = include_str!("../sql/receipt/get.sql");
pub(crate) const QUERY_SQL: &str = include_str!("../sql/receipt/query_created_at_ascending.sql");

pub(crate) async fn get(
    connection: &mut WamnConnection,
    id: wamn_postgres_sqlx::Uuid,
) -> Result<Option<ReceiptRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, ReceiptRow>(GET_SQL)
        .bind(id)
        .fetch_optional(connection)
        .await
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut WamnConnection,
    cursor_key: Option<wamn_postgres_sqlx::TimestampTz>,
    cursor_id: Option<wamn_postgres_sqlx::Uuid>,
    limit: i64,
) -> Result<Vec<ReceiptRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, ReceiptRow>(QUERY_SQL)
        .bind(cursor_key)
        .bind(cursor_id)
        .bind(limit)
        .fetch_all(connection)
        .await
}

// @generated from migration IR; do not edit.

use sqlx_core::query_as::query_as;
use wamn_postgres_sqlx::{WamnConnection, WamnPostgres};

#[derive(Debug, sqlx::FromRow)]
pub struct ReceiptLineRow {
    pub id: wamn_postgres_sqlx::Uuid,
    pub location_id: wamn_postgres_sqlx::Uuid,
    pub purchase_order_line_id: wamn_postgres_sqlx::Uuid,
    pub quantity: wamn_postgres_sqlx::Numeric,
    pub receipt_id: wamn_postgres_sqlx::Uuid,
}

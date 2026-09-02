// @generated from migration IR; do not edit.

use sqlx_core::query_as::query_as;
use wamn_postgres_sqlx::{WamnConnection, WamnPostgres};

#[derive(Debug, sqlx::FromRow)]
pub struct PurchaseOrderLineRow {
    pub id: wamn_postgres_sqlx::Uuid,
    pub item_id: wamn_postgres_sqlx::Uuid,
    pub line_number: i32,
    pub ordered_quantity: wamn_postgres_sqlx::Numeric,
    pub purchase_order_id: wamn_postgres_sqlx::Uuid,
    pub received_quantity: wamn_postgres_sqlx::Numeric,
}

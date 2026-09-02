// @generated from migration IR; do not edit.

use sqlx_core::query_as::query_as;
use sqlx_core::transaction::Transaction;
use wamn_postgres_sqlx::WamnPostgres;

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct LoadPurchaseOrderDetailRow {
    pub id: wamn_postgres_sqlx::Uuid,
    pub purchase_order_number: String,
    pub supplier_id: wamn_postgres_sqlx::Uuid,
    pub status: String,
    pub row_version: i64,
    pub acme_inspection_required: bool,
    pub acme_quality_status: String,
}

pub(crate) const LOAD_PURCHASE_ORDER_DETAIL_SQL: &str = include_str!("../../query/quality_purchase_order_detail.sql");

pub(crate) async fn load_purchase_order_detail(
    transaction: &mut Transaction<'_, WamnPostgres>,
    purchase_order_id: wamn_postgres_sqlx::Uuid,
) -> Result<Option<LoadPurchaseOrderDetailRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, LoadPurchaseOrderDetailRow>(LOAD_PURCHASE_ORDER_DETAIL_SQL)
        .bind(purchase_order_id)
        .fetch_optional(&mut **transaction)
        .await
}

// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct LoadPurchaseOrderDetailRow {
    pub id: uuid::Uuid,
    pub purchase_order_number: String,
    pub supplier_id: uuid::Uuid,
    pub status: String,
    pub row_version: i64,
    pub acme_inspection_required: bool,
    pub acme_quality_status: String,
}

pub(crate) const LOAD_PURCHASE_ORDER_DETAIL_SQL: &str = include_str!("../../query/quality_purchase_order_detail.sql");

pub(crate) fn load_purchase_order_detail_purchase_order_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

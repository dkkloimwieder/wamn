// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct LoadPurchaseOrderHistoryRow {
    pub position: i64,
    pub kind: String,
    pub operation: String,
    pub changed_by: uuid::Uuid,
    pub changed_at: chrono::DateTime<chrono::Utc>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub current: Option<String>,
}

pub(crate) const LOAD_PURCHASE_ORDER_HISTORY_SQL: &str =
    include_str!("../../query/load_purchase_order_history.sql");

pub(crate) fn load_purchase_order_history_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn load_purchase_order_history_after_position_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn load_purchase_order_history_limit_bind_fixture() -> i32 {
    0_i32
}

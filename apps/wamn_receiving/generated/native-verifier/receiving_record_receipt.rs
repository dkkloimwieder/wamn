// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct FinishPurchaseOrderRow {
    pub status: String,
    pub row_version: i32,
}

#[derive(Debug, sqlx::FromRow)]
pub struct InsertReceiptRow {
    pub id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct InsertReceiptLineRow {
    pub id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct LockPurchaseOrderRow {
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct UpdatePurchaseOrderLineRow {
    pub id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct ValidateReceiptLineRow {
    pub outcome: Option<String>,
    pub id: Option<uuid::Uuid>,
}

pub(crate) const FINISH_PURCHASE_ORDER_SQL: &str =
    include_str!("../../command/record_receipt/finish_purchase_order.sql");
pub(crate) const INSERT_RECEIPT_SQL: &str =
    include_str!("../../command/record_receipt/insert_receipt.sql");
pub(crate) const INSERT_RECEIPT_LINE_SQL: &str =
    include_str!("../../command/record_receipt/insert_receipt_line.sql");
pub(crate) const LOCK_PURCHASE_ORDER_SQL: &str =
    include_str!("../../command/record_receipt/lock_purchase_order.sql");
pub(crate) const UPDATE_PURCHASE_ORDER_LINE_SQL: &str =
    include_str!("../../command/record_receipt/update_purchase_order_line.sql");
pub(crate) const VALIDATE_RECEIPT_LINE_SQL: &str =
    include_str!("../../command/record_receipt/validate_receipt_line.sql");

pub(crate) fn finish_purchase_order_purchase_order_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_receipt_purchase_order_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_receipt_receipt_reference_bind_fixture() -> String {
    String::new()
}
pub(crate) fn insert_receipt_occurred_at_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn insert_receipt_line_receipt_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_receipt_line_line_bind_fixture() -> serde_json::Value {
    serde_json::Value::Null
}
pub(crate) fn lock_purchase_order_purchase_order_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn update_purchase_order_line_purchase_order_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn update_purchase_order_line_line_bind_fixture() -> serde_json::Value {
    serde_json::Value::Null
}
pub(crate) fn validate_receipt_line_purchase_order_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn validate_receipt_line_line_bind_fixture() -> serde_json::Value {
    serde_json::Value::Null
}

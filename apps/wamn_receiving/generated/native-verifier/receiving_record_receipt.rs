// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ClaimCommandRow {
    pub receipt_id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FinalizeCommandRow {
    pub purchase_order_status: Option<String>,
    pub row_version: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub receipt_id: uuid::Uuid,
    pub purchase_order_id: uuid::Uuid,
    pub purchase_order_status: Option<String>,
    pub row_version: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FinishPurchaseOrderRow {
    pub status: String,
    pub row_version: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct InsertReceiptRow {
    pub id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct InsertReceiptLineRow {
    pub id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct LockPurchaseOrderRow {
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct UpdatePurchaseOrderLineRow {
    pub id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ValidateReceiptLineRow {
    pub outcome: Option<String>,
    pub id: Option<uuid::Uuid>,
}

pub(crate) const CLAIM_COMMAND_SQL: &str = include_str!("../../command/record_receipt/claim_command.sql");
pub(crate) const FINALIZE_COMMAND_SQL: &str = include_str!("../../command/record_receipt/finalize_command.sql");
pub(crate) const FIND_REPLAY_SQL: &str = include_str!("../../command/record_receipt/find_replay.sql");
pub(crate) const FINISH_PURCHASE_ORDER_SQL: &str = include_str!("../../command/record_receipt/finish_purchase_order.sql");
pub(crate) const INSERT_RECEIPT_SQL: &str = include_str!("../../command/record_receipt/insert_receipt.sql");
pub(crate) const INSERT_RECEIPT_LINE_SQL: &str = include_str!("../../command/record_receipt/insert_receipt_line.sql");
pub(crate) const LOCK_PURCHASE_ORDER_SQL: &str = include_str!("../../command/record_receipt/lock_purchase_order.sql");
pub(crate) const UPDATE_PURCHASE_ORDER_LINE_SQL: &str = include_str!("../../command/record_receipt/update_purchase_order_line.sql");
pub(crate) const VALIDATE_RECEIPT_LINE_SQL: &str = include_str!("../../command/record_receipt/validate_receipt_line.sql");

pub(crate) fn claim_command_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn claim_command_canonical_command_bind_fixture() -> Vec<u8> {
    Vec::new()
}
pub(crate) fn claim_command_purchase_order_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn finalize_command_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn finalize_command_canonical_command_bind_fixture() -> Vec<u8> {
    Vec::new()
}
pub(crate) fn finalize_command_receipt_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn finalize_command_purchase_order_status_bind_fixture() -> String {
    String::new()
}
pub(crate) fn finalize_command_row_version_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn find_replay_idempotency_key_bind_fixture() -> String {
    String::new()
}
pub(crate) fn finish_purchase_order_purchase_order_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_receipt_receipt_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn insert_receipt_idempotency_key_bind_fixture() -> String {
    String::new()
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

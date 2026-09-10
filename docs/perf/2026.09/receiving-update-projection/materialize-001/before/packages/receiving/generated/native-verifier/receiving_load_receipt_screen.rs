// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct LoadReceiptScreenRow {
    pub purchase_order_id: uuid::Uuid,
    pub purchase_order_number: String,
    pub purchase_order_status: String,
    pub supplier_id: uuid::Uuid,
    pub row_version: i64,
    pub line_id: Option<uuid::Uuid>,
    pub line_number: Option<i32>,
    pub item_id: Option<uuid::Uuid>,
    pub item_number: Option<String>,
    pub ordered_quantity: Option<rust_decimal::Decimal>,
    pub received_quantity: Option<rust_decimal::Decimal>,
    pub remaining_quantity: Option<rust_decimal::Decimal>,
}

pub(crate) const LOAD_RECEIPT_SCREEN_SQL: &str = include_str!("../../query/load_receipt_screen.sql");

pub(crate) fn load_receipt_screen_purchase_order_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}

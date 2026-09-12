// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct ReceiptLineRow {
    pub id: uuid::Uuid,
    pub location_id: uuid::Uuid,
    pub purchase_order_line_id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
    pub receipt_id: uuid::Uuid,
}

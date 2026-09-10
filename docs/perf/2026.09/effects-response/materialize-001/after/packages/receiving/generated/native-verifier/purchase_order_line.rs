// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct PurchaseOrderLineRow {
    pub id: uuid::Uuid,
    pub item_id: uuid::Uuid,
    pub line_number: i32,
    pub ordered_quantity: rust_decimal::Decimal,
    pub purchase_order_id: uuid::Uuid,
    pub received_quantity: rust_decimal::Decimal,
}

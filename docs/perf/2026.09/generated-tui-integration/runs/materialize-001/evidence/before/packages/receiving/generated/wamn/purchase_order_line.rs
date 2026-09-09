// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct PurchaseOrderLineRow {
    pub id: wamn_postgres_statements::Uuid,
    pub item_id: wamn_postgres_statements::Uuid,
    pub line_number: i32,
    pub ordered_quantity: wamn_postgres_statements::Numeric,
    pub purchase_order_id: wamn_postgres_statements::Uuid,
    pub received_quantity: wamn_postgres_statements::Numeric,
}

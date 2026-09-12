// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct InventoryMovementRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub from_location_id: Option<wamn_postgres_statements::Uuid>,
    pub id: wamn_postgres_statements::Uuid,
    pub idempotency_key: String,
    pub kind: String,
    pub occurred_at: wamn_postgres_statements::TimestampTz,
    pub pallet_id: wamn_postgres_statements::Uuid,
    pub product_id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
    pub reason_code: Option<String>,
    pub to_location_id: Option<wamn_postgres_statements::Uuid>,
}

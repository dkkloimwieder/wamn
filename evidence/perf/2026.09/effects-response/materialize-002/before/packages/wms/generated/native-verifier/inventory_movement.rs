// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct InventoryMovementRow {
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub from_location_id: Option<uuid::Uuid>,
    pub id: uuid::Uuid,
    pub idempotency_key: String,
    pub kind: String,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub pallet_id: uuid::Uuid,
    pub product_id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
    pub reason_code: Option<String>,
    pub to_location_id: Option<uuid::Uuid>,
}

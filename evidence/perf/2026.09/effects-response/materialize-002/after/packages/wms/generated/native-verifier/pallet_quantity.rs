// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct PalletQuantityRow {
    pub id: uuid::Uuid,
    pub pallet_id: uuid::Uuid,
    pub product_id: uuid::Uuid,
    pub quantity: rust_decimal::Decimal,
    pub status: String,
}

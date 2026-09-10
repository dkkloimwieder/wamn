// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct InventoryAggregateRow {
    pub product_id: uuid::Uuid,
    pub location_id: uuid::Uuid,
    pub status: String,
    pub quantity: rust_decimal::Decimal,
    pub pallet_count: i64,
}

pub(crate) const INVENTORY_AGGREGATE_SQL: &str = include_str!("../../query/inventory_aggregate.sql");

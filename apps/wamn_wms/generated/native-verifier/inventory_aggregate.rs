// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct InventoryAggregateRow {
    pub product_id: uuid::Uuid,
    pub location_id: uuid::Uuid,
    pub status: String,
    pub quantity: Option<rust_decimal::Decimal>,
    pub pallet_count: Option<i32>,
}

pub(crate) const INVENTORY_AGGREGATE_SQL: &str =
    include_str!("../../query/inventory_aggregate.sql");

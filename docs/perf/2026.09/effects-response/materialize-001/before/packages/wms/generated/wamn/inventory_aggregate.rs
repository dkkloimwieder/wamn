// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct InventoryAggregateRow {
    pub product_id: wamn_postgres_statements::Uuid,
    pub location_id: wamn_postgres_statements::Uuid,
    pub status: String,
    pub quantity: wamn_postgres_statements::Numeric,
    pub pallet_count: i64,
}

pub(crate) const INVENTORY_AGGREGATE_DIGEST: &str = "sha256:51442e209ad8e0f13c6221274cf9e2a1cc9f615cc78976ecbe61174c8e3b068d";

pub(crate) async fn inventory_aggregate(
    transaction: &mut Transaction,
) -> Result<Vec<InventoryAggregateRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(INVENTORY_AGGREGATE_DIGEST, vec![
    ]).await?;
    wamn_postgres_statements::decode_all(INVENTORY_AGGREGATE_DIGEST, rows, |row| {
        Ok(InventoryAggregateRow {
            product_id: row.decode("product_id")?,
            location_id: row.decode("location_id")?,
            status: row.decode("status")?,
            quantity: row.decode("quantity")?,
            pallet_count: row.decode("pallet_count")?,
        })
    })
}

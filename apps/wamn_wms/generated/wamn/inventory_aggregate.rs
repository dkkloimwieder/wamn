// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct InventoryAggregateRow {
    pub product_id: wamn_postgres_statements::Uuid,
    pub location_id: wamn_postgres_statements::Uuid,
    pub disposition: String,
    pub quantity: Option<wamn_postgres_statements::Numeric>,
    pub packaging_count: Option<i32>,
}

pub(crate) const INVENTORY_AGGREGATE_DIGEST: &str =
    "sha256:d69f4ccb82c417503e4f33b6e11bf78bbcded15fb8ab5342c04b880fcec24304";

pub(crate) async fn inventory_aggregate(
    transaction: &mut Transaction,
) -> Result<Vec<InventoryAggregateRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(INVENTORY_AGGREGATE_DIGEST, vec![]).await?;
    wamn_postgres_statements::decode_all(INVENTORY_AGGREGATE_DIGEST, rows, |row| {
        Ok(InventoryAggregateRow {
            product_id: row.decode("product_id")?,
            location_id: row.decode("location_id")?,
            disposition: row.decode("disposition")?,
            quantity: row.decode("quantity")?,
            packaging_count: row.decode("packaging_count")?,
        })
    })
}

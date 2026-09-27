// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct InventoryAggregateRow {
    pub product_id: wamn_postgres_statements::Uuid,
    pub location_id: wamn_postgres_statements::Uuid,
    pub status: String,
    pub quantity: Option<wamn_postgres_statements::Numeric>,
    pub packaging_count: Option<i32>,
}

pub(crate) const INVENTORY_AGGREGATE_DIGEST: &str =
    "sha256:afa504b98bb048725b1024fd0a81f0500541d9954e63e9cd4b0948153f21638f";

pub(crate) async fn inventory_aggregate(
    transaction: &mut Transaction,
) -> Result<Vec<InventoryAggregateRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(INVENTORY_AGGREGATE_DIGEST, vec![]).await?;
    wamn_postgres_statements::decode_all(INVENTORY_AGGREGATE_DIGEST, rows, |row| {
        Ok(InventoryAggregateRow {
            product_id: row.decode("product_id")?,
            location_id: row.decode("location_id")?,
            status: row.decode("status")?,
            quantity: row.decode("quantity")?,
            packaging_count: row.decode("packaging_count")?,
        })
    })
}

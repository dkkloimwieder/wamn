//! Open inventory totals by product, explicit location, and disposition.
use wamn_postgres_statements::Connection;

use crate::error::{self, AccessError};
use crate::generated::wamn::inventory_aggregate as sql;

pub use crate::generated::wamn::inventory_aggregate::InventoryAggregateRow;

/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn execute(
    connection: &mut Connection,
) -> Result<Vec<InventoryAggregateRow>, AccessError> {
    let mut transaction = connection
        .begin()
        .await
        .map_err(|e| error::from_statement(&e))?;
    let rows = sql::inventory_aggregate(&mut transaction)
        .await
        .map_err(|e| error::from_statement(&e))?;
    transaction
        .commit()
        .await
        .map_err(|e| error::from_statement(&e))?;
    Ok(rows)
}

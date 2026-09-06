//! `inventory.aggregate` -- live stock by product, location and status.
//!
//! One authored statement (`query/inventory_aggregate.sql`), which excludes
//! consumed pallets and says why. The result is the bounded list the contract
//! declares; the input carries nothing but its correlation id.

use serde::Deserialize;
use serde_json::{Value, json};
use wamn_postgres_statements::Connection;

use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::inventory_aggregate as sql;

pub(crate) const REFUSALS: &[AccessErrorKind] = &[
    AccessErrorKind::InvalidInput,
    AccessErrorKind::Retry,
    AccessErrorKind::Timeout,
    AccessErrorKind::PermissionDenied,
    AccessErrorKind::InternalError,
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AggregateInput {}

/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub(crate) async fn execute(
    connection: &mut Connection,
) -> Result<Vec<sql::InventoryAggregateRow>, AccessError> {
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

pub(crate) fn rows_to_json(rows: &[sql::InventoryAggregateRow]) -> Value {
    json!({
        "rows": rows
            .iter()
            .map(|row| {
                json!({
                    "product_id": row.product_id.0,
                    "location_id": row.location_id.0,
                    "status": row.status,
                    "quantity": row.quantity.0,
                    "pallet_count": row.pallet_count,
                })
            })
            .collect::<Vec<_>>(),
    })
}

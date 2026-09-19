//! `inventory.aggregate` -- live stock by product, location and status.
//!
//! One authored statement (`query/inventory_aggregate.sql`), which excludes
//! consumed pallets and says why. The result is the bounded list the contract
//! declares; the input carries nothing but its correlation id.

use wamn_postgres_statements::Connection;

#[cfg(test)]
use crate::error::AccessErrorKind;
use crate::error::{self, AccessError};
use crate::generated::wamn::inventory_aggregate as sql;

pub use crate::generated::wamn::inventory_aggregate::InventoryAggregateRow;

/// What `inventory.aggregate` can refuse with. Read only by the contract test in
/// `error`, which holds this list to the operation's generated contract.
#[cfg(test)]
pub(crate) const REFUSALS: &[AccessErrorKind] = &[
    AccessErrorKind::InvalidInput,
    AccessErrorKind::Retry,
    AccessErrorKind::Timeout,
    AccessErrorKind::PermissionDenied,
    AccessErrorKind::InternalError,
];

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

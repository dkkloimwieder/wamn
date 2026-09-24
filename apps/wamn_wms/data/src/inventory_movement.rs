//! `inventory_movement` get and query over the generated statements.

use wamn_postgres_statements::Connection;

use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::inventory_movement as sql;
use crate::page::{self, Page};
use crate::scalar;

pub use crate::generated::wamn::inventory_movement::InventoryMovementRow;

/// Load one inventory movement by id.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn get(
    connection: &mut Connection,
    id: &str,
) -> Result<InventoryMovementRow, AccessError> {
    let id = scalar::uuid("id", id)?;
    sql::get(connection, id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(|| AccessError::missing(AccessErrorKind::NotFound, "id", &id.0))
}

/// Query one bounded page.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn query(
    connection: &mut Connection,
    cursor: Option<&str>,
    limit: Option<i64>,
) -> Result<Page<InventoryMovementRow>, AccessError> {
    let ((key, id), limit) = page::start(cursor, limit)?;
    let rows = sql::query_created_at_ascending(connection, key, id, limit + 1)
        .await
        .map_err(|e| error::from_statement(&e))?;
    page::finish(rows, limit, |row| (&row.created_at, &row.id))
}

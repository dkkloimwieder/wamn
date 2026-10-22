//! The `widget_maker.list` projection.

use wamn_postgres_statements::{StatementError, Transaction};

use crate::error::{AccessError, Constraints};
use crate::statements::wamn::widget_maker_list;

#[doc(inline)]
pub use crate::statements::wamn::widget_maker_list::ListRow;

/// List every widget maker in name order.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn list(transaction: &mut Transaction) -> Result<Vec<ListRow>, AccessError> {
    let statement = |error: StatementError| AccessError::from_statement(&error, Constraints::NONE);
    let rows = widget_maker_list::list(transaction)
        .await
        .map_err(statement)?;
    Ok(rows)
}

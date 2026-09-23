//! The `widget_maker` model query and the `widget_maker.list` projection.

use wamn_postgres_statements::{Connection, StatementError};

use crate::error::{AccessError, Constraints};
use crate::generated::wamn::{widget_maker as sql, widget_maker_list};
use crate::page::{self, Page, QueryInput};

#[doc(inline)]
pub use crate::generated::wamn::widget_maker::WidgetMakerRow;
#[doc(inline)]
pub use crate::generated::wamn::widget_maker_list::ListRow;

/// Query one bounded page, filtered by `name`.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn query(
    connection: &mut Connection,
    input: &QueryInput,
) -> Result<Page<WidgetMakerRow>, AccessError> {
    let plan = page::plan(input)?;
    let (filter, key, id) = (
        plan.filter.clone(),
        plan.cursor_key.clone(),
        plan.cursor_id.clone(),
    );
    let rows = if plan.descending {
        sql::query_created_at_descending(connection, filter, key, id, plan.fetch).await
    } else {
        sql::query_created_at_ascending(connection, filter, key, id, plan.fetch).await
    }
    .map_err(|error| AccessError::from_statement(&error, Constraints::NONE))?;
    page::finish(&plan, rows, |row| (&row.created_at, &row.id))
}

/// List every widget maker in name order.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn list(connection: &mut Connection) -> Result<Vec<ListRow>, AccessError> {
    let statement = |error: StatementError| AccessError::from_statement(&error, Constraints::NONE);
    let mut transaction = connection.begin().await.map_err(statement)?;
    let rows = widget_maker_list::list(&mut transaction)
        .await
        .map_err(statement)?;
    transaction.commit().await.map_err(statement)?;
    Ok(rows)
}

//! The three custom `widget` operations.

use wamn_postgres_statements::{Connection, StatementError, Transaction};

use crate::error::{AccessError, AccessErrorType, Constraints};
use crate::scalar;
use crate::statements::wamn::{widget_archive, widget_list, widget_record_batch as batch_sql};

#[doc(inline)]
pub use crate::statements::wamn::widget_archive::ArchiveRow;
#[doc(inline)]
pub use crate::statements::wamn::widget_list::ListRow;
#[doc(inline)]
pub use crate::statements::wamn::widget_record_batch::FindWidgetRow;

const MAX_BATCH_LINES: usize = 10;

/// One `widget.record_batch` line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Line {
    pub widget_id: String,
    pub amount: String,
}

/// The `widget.record_batch` command value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Batch {
    pub note: Option<String>,
    pub maker_id: Option<String>,
    pub line: Vec<Line>,
}

/// Lock one widget and confirm the revision the caller last read.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn archive(
    transaction: &mut Transaction,
    id: &str,
    expected_edit_version: i64,
) -> Result<ArchiveRow, AccessError> {
    let id = scalar::uuid("id", id)?;
    let statement = |error: StatementError| AccessError::from_statement(&error, Constraints::NONE);
    let row = widget_archive::archive(transaction, id)
        .await
        .map_err(statement)?;
    if row.edit_version != expected_edit_version {
        return Err(AccessError::conflict(
            expected_edit_version,
            row.edit_version,
        ));
    }
    Ok(row)
}

/// List every widget in id order.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn list(connection: &mut Connection) -> Result<Vec<ListRow>, AccessError> {
    let statement = |error: StatementError| AccessError::from_statement(&error, Constraints::NONE);
    let mut transaction = connection.begin().await.map_err(statement)?;
    let rows = widget_list::list(&mut transaction)
        .await
        .map_err(statement)?;
    transaction.commit().await.map_err(statement)?;
    Ok(rows)
}

/// Record one batch of lines in the transaction the host began for the
/// operation, and answer the widget of its first line in `widget_id` order.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn record_batch(
    transaction: &mut Transaction,
    batch: &Batch,
) -> Result<FindWidgetRow, AccessError> {
    let count = batch.line.len();
    if !(1..=MAX_BATCH_LINES).contains(&count) {
        return Err(AccessError::range(
            "value.line[]",
            1,
            i64::try_from(MAX_BATCH_LINES).expect("the line maximum fits i64"),
            i64::try_from(count).unwrap_or(i64::MAX),
        ));
    }
    let mut lines = batch
        .line
        .iter()
        .map(|line| {
            Ok((
                scalar::uuid("value.line[].widget_id", &line.widget_id)?.0,
                scalar::positive_numeric("value.line[].amount", &line.amount)?.0,
            ))
        })
        .collect::<Result<Vec<_>, AccessError>>()?;
    lines.sort();
    if lines.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(AccessError::field(
            AccessErrorType::InvalidInput,
            "value.line[].widget_id",
        ));
    }
    if let Some(maker_id) = batch.maker_id.as_deref() {
        scalar::uuid("value.maker_id", maker_id)?;
    }
    let (widget_id, _) = lines.swap_remove(0);
    batch_sql::find_widget(transaction, wamn_postgres_statements::Uuid(widget_id))
        .await
        .map_err(|error| AccessError::from_statement(&error, Constraints::NONE))
}

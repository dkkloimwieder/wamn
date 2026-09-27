//! The `widget` model operations and the three custom `widget` operations.

use wamn_postgres_statements::{Connection, StatementError, Transaction};

use crate::error::{AccessError, AccessErrorKind, Constraints};
use crate::generated::wamn::{
    widget as sql, widget_archive, widget_list, widget_record_batch as batch_sql,
};
use crate::page::{self, Page, QueryInput};
use crate::scalar;

#[doc(inline)]
pub use crate::generated::wamn::widget::{WidgetDeleteRow, WidgetRow};
#[doc(inline)]
pub use crate::generated::wamn::widget_archive::ArchiveRow;
#[doc(inline)]
pub use crate::generated::wamn::widget_list::ListRow;
#[doc(inline)]
pub use crate::generated::wamn::widget_record_batch::FindWidgetRow;

const CREATE: Constraints = Constraints {
    unique: sql::CREATE_UNIQUE_CONSTRAINTS,
    foreign_key: sql::CREATE_FOREIGN_KEY_CONSTRAINTS,
    check: sql::CREATE_CHECK_CONSTRAINTS,
    exclusion: sql::CREATE_EXCLUSION_CONSTRAINTS,
};
pub(crate) const UPDATE: Constraints = Constraints {
    unique: sql::UPDATE_UNIQUE_CONSTRAINTS,
    foreign_key: sql::UPDATE_FOREIGN_KEY_CONSTRAINTS,
    check: sql::UPDATE_CHECK_CONSTRAINTS,
    exclusion: sql::UPDATE_EXCLUSION_CONSTRAINTS,
};
const MAX_BATCH_LINES: usize = 10;

/// One `widget.update` change. An outer `None` leaves the field as it is.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Change {
    pub code: Option<Option<String>>,
    pub maker_id: Option<Option<String>>,
    pub note: Option<Option<String>>,
}

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

/// Load one widget by id.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn get(connection: &mut Connection, id: &str) -> Result<WidgetRow, AccessError> {
    let id = scalar::uuid("id", id)?;
    sql::get(connection, id.clone())
        .await
        .map_err(|error| AccessError::from_statement(&error, Constraints::NONE))?
        .ok_or_else(|| AccessError::missing(&id.0))
}

/// Query one bounded page, filtered by `code`, by the start of `note`, and by
/// whether `maker_id` is empty.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn query(
    connection: &mut Connection,
    input: &QueryInput,
) -> Result<Page<WidgetRow>, AccessError> {
    let plan = page::plan(input)?;
    let (filter, prefix, empty, key, id) = (
        plan.filter.clone(),
        plan.prefix.clone(),
        plan.empty.clone(),
        plan.cursor_key.clone(),
        plan.cursor_id.clone(),
    );
    let rows = if plan.descending {
        sql::query_created_at_descending(connection, filter, prefix, empty, key, id, plan.limit + 1)
            .await
    } else {
        sql::query_created_at_ascending(connection, filter, prefix, empty, key, id, plan.limit + 1)
            .await
    }
    .map_err(|error| AccessError::from_statement(&error, Constraints::NONE))?;
    let descending = plan.descending;
    Ok(Page::new(rows, plan.limit, move |row| {
        page::cursor(descending, &row.created_at, &row.id)
    }))
}

/// Create one widget in the transaction its codec holds for the write log.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn create(
    transaction: &mut Transaction,
    code: Option<&str>,
    maker_id: Option<&str>,
    note: Option<&str>,
) -> Result<WidgetRow, AccessError> {
    // The column is NOT NULL and has no default.
    let code = code.ok_or_else(|| AccessError::field(AccessErrorKind::InvalidInput, "code"))?;
    let maker_id = maker_id
        .map(|value| scalar::uuid("maker_id", value))
        .transpose()?;
    sql::create(
        transaction,
        code.to_owned(),
        maker_id,
        note.map(str::to_owned),
    )
    .await
    .map_err(|error| AccessError::from_statement(&error, CREATE))
}

/// Change one widget at the revision the caller last read.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn update(
    connection: &mut Connection,
    id: &str,
    expected_edit_version: i64,
    change: Change,
) -> Result<WidgetRow, AccessError> {
    let id = scalar::uuid("id", id)?;
    if matches!(change.code, Some(None)) {
        return Err(AccessError::field(
            AccessErrorKind::InvalidInput,
            "change.code",
        ));
    }
    let maker_id = change
        .maker_id
        .map(|value| {
            value
                .map(|value| scalar::uuid("change.maker_id", &value))
                .transpose()
        })
        .transpose()?;
    let row = sql::update(
        connection,
        id.clone(),
        expected_edit_version,
        change.code.is_some(),
        change.code.flatten(),
        maker_id.is_some(),
        maker_id.flatten(),
        change.note.is_some(),
        change.note.flatten(),
    )
    .await
    .map_err(|error| AccessError::from_statement(&error, UPDATE))?;
    match row.outcome.as_deref() {
        Some("updated") => Ok(WidgetRow {
            code: row.code.ok_or_else(AccessError::internal)?,
            created_at: row.created_at.ok_or_else(AccessError::internal)?,
            edit_version: row.edit_version.ok_or_else(AccessError::internal)?,
            id: row.id.ok_or_else(AccessError::internal)?,
            maker_id: row.maker_id,
            note: row.note,
        }),
        Some("not_found") => Err(AccessError::missing(&id.0)),
        Some("concurrency_conflict") => Err(AccessError::conflict(
            expected_edit_version,
            row.observed_edit_version
                .ok_or_else(AccessError::internal)?,
        )),
        _ => Err(AccessError::internal()),
    }
}

/// Delete one widget at the revision the caller last read.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn delete(
    connection: &mut Connection,
    id: &str,
    expected_edit_version: i64,
) -> Result<WidgetDeleteRow, AccessError> {
    let id = scalar::uuid("id", id)?;
    let statement = |error: StatementError| AccessError::from_statement(&error, Constraints::NONE);
    let row = sql::delete(connection, id.clone(), expected_edit_version)
        .await
        .map_err(statement)?;
    match row.outcome.as_deref() {
        Some("deleted") => Ok(row),
        Some("not_found") => Err(AccessError::missing(&id.0)),
        // The delete statement returns no revision, so a second read supplies
        // the one the conflict reports.
        Some("concurrency_conflict") => {
            match sql::get(connection, id.clone()).await.map_err(statement)? {
                Some(current) => Err(AccessError::conflict(
                    expected_edit_version,
                    current.edit_version,
                )),
                None => Err(AccessError::missing(&id.0)),
            }
        }
        _ => Err(AccessError::internal()),
    }
}

/// Lock one widget and confirm the revision the caller last read.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn archive(
    connection: &mut Connection,
    id: &str,
    expected_edit_version: i64,
) -> Result<ArchiveRow, AccessError> {
    let id = scalar::uuid("id", id)?;
    let statement = |error: StatementError| AccessError::from_statement(&error, Constraints::NONE);
    let mut transaction = connection.begin().await.map_err(statement)?;
    let row = widget_archive::archive(&mut transaction, id)
        .await
        .map_err(statement)?;
    if row.edit_version != expected_edit_version {
        return Err(AccessError::conflict(
            expected_edit_version,
            row.edit_version,
        ));
    }
    transaction.commit().await.map_err(statement)?;
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

/// Record one batch of lines in the transaction its codec holds for the write
/// log, and answer the widget of its first line in `widget_id` order.
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
            AccessErrorKind::InvalidInput,
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

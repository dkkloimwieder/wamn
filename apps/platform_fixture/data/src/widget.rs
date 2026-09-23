//! The `widget` model operations and the three custom `widget` operations.

use serde_json::json;
use wamn_execution_contract::canonical_json_bytes;
use wamn_postgres_statements::{Connection, StatementError};

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
pub use crate::generated::wamn::widget_record_batch::FinalizeBatchRow;

const CREATE: Constraints = Constraints {
    unique: sql::CREATE_UNIQUE_CONSTRAINTS,
    foreign_key: sql::CREATE_FOREIGN_KEY_CONSTRAINTS,
    check: sql::CREATE_CHECK_CONSTRAINTS,
};
const UPDATE: Constraints = Constraints {
    unique: sql::UPDATE_UNIQUE_CONSTRAINTS,
    foreign_key: sql::UPDATE_FOREIGN_KEY_CONSTRAINTS,
    check: sql::UPDATE_CHECK_CONSTRAINTS,
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
    pub idempotency_key: String,
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

/// Query one bounded page, filtered by `code`.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn query(
    connection: &mut Connection,
    input: &QueryInput,
) -> Result<Page<WidgetRow>, AccessError> {
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

/// Create one widget under the identity its claim row mints.
///
/// A retry under the same key with the same fields returns the row the first
/// attempt wrote. The same key with other fields is refused.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn create(
    connection: &mut Connection,
    idempotency_key: &str,
    code: Option<&str>,
    maker_id: Option<&str>,
    note: Option<&str>,
) -> Result<WidgetRow, AccessError> {
    // The column is NOT NULL and has no default.
    let code = code.ok_or_else(|| AccessError::field(AccessErrorKind::InvalidInput, "code"))?;
    let maker_id = maker_id
        .map(|value| scalar::uuid("maker_id", value))
        .transpose()?;
    let canonical_command = canonical_json_bytes(&json!({
        "code": code,
        "maker_id": maker_id.as_ref().map(|value| &value.0),
        "note": note,
    }));
    let statement = |error: StatementError| AccessError::from_statement(&error, CREATE);
    let mut claim = sql::begin_claim(connection.begin().await.map_err(statement)?);
    if let Some(replay) = sql::create_replay(&mut claim, idempotency_key.to_owned())
        .await
        .map_err(statement)?
    {
        return replayed(replay, &canonical_command);
    }
    let claimed = sql::create_claim(
        &mut claim,
        idempotency_key.to_owned(),
        canonical_command.clone(),
    )
    .await
    .map_err(statement)?;
    let Some(claimed) = claimed else {
        // Another caller took the key between the replay read and the claim.
        // Its row is committed, so the replay read now answers.
        let replay = sql::create_replay(&mut claim, idempotency_key.to_owned())
            .await
            .map_err(statement)?
            .ok_or_else(AccessError::internal)?;
        return replayed(replay, &canonical_command);
    };
    let finalized = sql::create(
        claim,
        claimed.widget_id,
        code.to_owned(),
        maker_id,
        note.map(str::to_owned),
    )
    .await
    .map_err(statement)?;
    let row = WidgetRow {
        code: finalized.row.code.clone(),
        created_at: finalized.row.created_at.clone(),
        edit_version: finalized.row.edit_version,
        id: finalized.row.id.clone(),
        maker_id: finalized.row.maker_id.clone(),
        note: finalized.row.note.clone(),
    };
    finalized.commit().await.map_err(statement)?;
    Ok(row)
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

/// Record one batch of lines under one idempotency key.
///
/// The command hashes its canonical form: the key and the request id left
/// out, and the lines in ascending `widget_id` order with positive amounts.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn record_batch(
    connection: &mut Connection,
    batch: &Batch,
) -> Result<FinalizeBatchRow, AccessError> {
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
    let maker_id = batch
        .maker_id
        .as_deref()
        .map(|value| scalar::uuid("value.maker_id", value))
        .transpose()?;
    let canonical_command = canonical_json_bytes(&json!({
        "line": lines
            .iter()
            .map(|(widget_id, amount)| json!({ "amount": amount, "widget_id": widget_id }))
            .collect::<Vec<_>>(),
        "maker_id": maker_id.map(|value| value.0),
        "note": batch.note,
    }));

    let key = &batch.idempotency_key;
    let statement = |error: StatementError| AccessError::from_statement(&error, Constraints::NONE);
    let mut claim = batch_sql::begin_claim(connection.begin().await.map_err(statement)?);
    if let Some(replay) = batch_sql::find_batch(&mut claim, key.clone())
        .await
        .map_err(statement)?
    {
        return batch_replayed(replay, &canonical_command);
    }
    let claimed = batch_sql::claim_batch(&mut claim, canonical_command.clone(), key.clone())
        .await
        .map_err(statement)?;
    if claimed.is_none() {
        let replay = batch_sql::find_batch(&mut claim, key.clone())
            .await
            .map_err(statement)?
            .ok_or_else(AccessError::internal)?;
        return batch_replayed(replay, &canonical_command);
    }
    let finalized = batch_sql::finalize_batch(claim, key.clone())
        .await
        .map_err(statement)?;
    let row = FinalizeBatchRow {
        widget_id: finalized.row.widget_id.clone(),
    };
    finalized.commit().await.map_err(statement)?;
    Ok(row)
}

fn replayed(
    replay: sql::WidgetCreateReplayRow,
    canonical_command: &[u8],
) -> Result<WidgetRow, AccessError> {
    if replay.canonical_command != canonical_command {
        return Err(AccessError::field(
            AccessErrorKind::IdempotencyConflict,
            "idempotency_key",
        ));
    }
    Ok(WidgetRow {
        code: replay.code,
        created_at: replay.created_at,
        edit_version: replay.edit_version,
        id: replay.id,
        maker_id: replay.maker_id,
        note: replay.note,
    })
}

fn batch_replayed(
    replay: batch_sql::FindBatchRow,
    canonical_command: &[u8],
) -> Result<FinalizeBatchRow, AccessError> {
    if replay.canonical_command != canonical_command {
        return Err(AccessError::field(
            AccessErrorKind::IdempotencyConflict,
            "value.idempotency_key",
        ));
    }
    Ok(FinalizeBatchRow {
        widget_id: replay.widget_id,
    })
}

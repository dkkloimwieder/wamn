// @generated from the package manifest and migration IR; do not edit.

// The generated `widget` operations.

#[allow(unused_imports)]
use wamn_data_access::{Direction, Invalid, Page, cursor, scalar};
use wamn_postgres_statements::Connection;

#[allow(unused_imports)]
use super::error::{Constraints, Error};

/// The statement accessors of the model.
pub mod sql {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wamn/widget.rs"
    ));
}

pub use sql::WidgetDeleteRow;
pub use sql::WidgetRow;
pub use sql::WidgetUpdateRow;

const GET_OPERATION: &str = "widget.get";
const QUERY_OPERATION: &str = "widget.query";
const CREATE_OPERATION: &str = "widget.create";
const UPDATE_OPERATION: &str = "widget.update";
const DELETE_OPERATION: &str = "widget.delete";

/// `widget.get`: load one row by id.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
pub async fn get(connection: &mut Connection, id: &str) -> Result<WidgetRow, Error> {
    let id = scalar::uuid(id).map_err(|Invalid| Error::invalid("id"))?;
    sql::get(connection, id.clone())
        .await
        .map_err(|error| Error::from_statement(&error, &Constraints::NONE, GET_OPERATION))?
        .ok_or_else(|| Error::not_found(&id))
}

/// The `widget.query` request, after its codec filled the limit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryInput {
    pub code: Option<Vec<String>>,
    pub note: Option<Vec<String>>,
    pub maker_id: Option<bool>,
    pub sort_field: Option<String>,
    pub sort_direction: Option<String>,
    pub cursor: Option<String>,
    pub limit: i64,
}

fn query_statement(error: &wamn_postgres_statements::StatementError) -> Error {
    Error::from_statement(error, &Constraints::NONE, QUERY_OPERATION)
}

/// `widget.query`: one keyset page in the requested sort.
///
/// The contract's order holds: the sort and the cursor first, then the
/// statement. The codec checked the limit.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
pub async fn query(
    connection: &mut Connection,
    input: QueryInput,
) -> Result<Page<WidgetRow, Error>, Error> {
    let sort = match (input.sort_field.as_deref(), input.sort_direction.as_deref()) {
        (None, None) => ("created_at", Direction::Ascending),
        (Some(field), Some("ascending")) => (field, Direction::Ascending),
        (Some(field), Some("descending")) => (field, Direction::Descending),
        _ => return Err(Error::invalid("sort")),
    };
    let code_filter = input.code.as_deref().map(scalar::json_list);
    let note_filter = input.note.as_deref().map(scalar::json_list);
    let maker_id_filter = input.maker_id.map(scalar::json_boolean);
    match sort {
        ("created_at", Direction::Ascending) => {
            let (cursor_key, cursor_id) = match input.cursor.as_deref() {
                Some(encoded) => {
                    let (key, id) = cursor::decode::<wamn_postgres_statements::TimestampTz>(
                        encoded,
                        "created_at",
                        Direction::Ascending,
                    )
                    .map_err(|Invalid| Error::invalid("cursor"))?;
                    (Some(key), Some(id))
                }
                None => (None, None),
            };
            let rows = sql::query_created_at_ascending(
                connection,
                code_filter,
                note_filter,
                maker_id_filter,
                cursor_key,
                cursor_id,
                input.limit + 1,
            )
            .await
            .map_err(|error| query_statement(&error))?;
            Ok(Page::new(rows, input.limit, query_statement, |row| {
                cursor::encode("created_at", Direction::Ascending, &row.created_at, &row.id)
                    .map_err(|Invalid| Error::internal())
            }))
        }
        ("created_at", Direction::Descending) => {
            let (cursor_key, cursor_id) = match input.cursor.as_deref() {
                Some(encoded) => {
                    let (key, id) = cursor::decode::<wamn_postgres_statements::TimestampTz>(
                        encoded,
                        "created_at",
                        Direction::Descending,
                    )
                    .map_err(|Invalid| Error::invalid("cursor"))?;
                    (Some(key), Some(id))
                }
                None => (None, None),
            };
            let rows = sql::query_created_at_descending(
                connection,
                code_filter,
                note_filter,
                maker_id_filter,
                cursor_key,
                cursor_id,
                input.limit + 1,
            )
            .await
            .map_err(|error| query_statement(&error))?;
            Ok(Page::new(rows, input.limit, query_statement, |row| {
                cursor::encode(
                    "created_at",
                    Direction::Descending,
                    &row.created_at,
                    &row.id,
                )
                .map_err(|Invalid| Error::internal())
            }))
        }

        _ => Err(Error::invalid("sort")),
    }
}

/// `widget.create`: create one row in the transaction the host began
/// for the operation.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    transaction: &mut wamn_postgres_statements::Transaction,
    code: Option<String>,
    maker_id: Option<String>,
    note: Option<String>,
) -> Result<WidgetRow, Error> {
    let Some(value) = code else {
        return Err(Error::invalid("code"));
    };
    let code = value;
    let maker_id = match maker_id {
        Some(value) => Some(scalar::uuid(&value).map_err(|Invalid| Error::invalid("maker_id"))?),
        None => None,
    };
    sql::create(transaction, code, maker_id, note)
        .await
        .map_err(|error| {
            Error::from_statement(
                &error,
                &Constraints {
                    unique: sql::CREATE_UNIQUE_CONSTRAINTS,
                    foreign_key: sql::CREATE_FOREIGN_KEY_CONSTRAINTS,
                    check: sql::CREATE_CHECK_CONSTRAINTS,
                    exclusion: sql::CREATE_EXCLUSION_CONSTRAINTS,
                },
                CREATE_OPERATION,
            )
        })
}

/// `widget.update`: change one row at the revision the caller last read.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
#[allow(clippy::too_many_arguments)]
pub async fn update(
    transaction: &mut wamn_postgres_statements::Transaction,
    id: &str,
    expected_edit_version: i64,
    code: Option<Option<String>>,
    maker_id: Option<Option<String>>,
    note: Option<Option<String>>,
) -> Result<WidgetRow, Error> {
    let id = scalar::uuid(id).map_err(|Invalid| Error::invalid("id"))?;
    if matches!(code, Some(None)) {
        return Err(Error::invalid("change.code"));
    }
    let code_present = code.is_some();
    let code = code.flatten();
    let maker_id_present = maker_id.is_some();
    let maker_id = match maker_id.flatten() {
        Some(value) => {
            Some(scalar::uuid(&value).map_err(|Invalid| Error::invalid("change.maker_id"))?)
        }
        None => None,
    };
    let note_present = note.is_some();
    let note = note.flatten();
    let row = sql::update(
        transaction,
        id.clone(),
        expected_edit_version,
        code_present,
        code,
        maker_id_present,
        maker_id,
        note_present,
        note,
    )
    .await
    .map_err(|error| {
        Error::from_statement(
            &error,
            &Constraints {
                unique: sql::UPDATE_UNIQUE_CONSTRAINTS,
                foreign_key: sql::UPDATE_FOREIGN_KEY_CONSTRAINTS,
                check: sql::UPDATE_CHECK_CONSTRAINTS,
                exclusion: sql::UPDATE_EXCLUSION_CONSTRAINTS,
            },
            UPDATE_OPERATION,
        )
    })?;
    match row.outcome.as_deref() {
        Some("updated") => Ok(WidgetRow {
            code: row.code.ok_or_else(Error::internal)?,
            created_at: row.created_at.ok_or_else(Error::internal)?,
            edit_version: row.edit_version.ok_or_else(Error::internal)?,
            id: row.id.ok_or_else(Error::internal)?,
            maker_id: row.maker_id,
            note: row.note,
        }),
        Some("not_found") => Err(Error::not_found(&id)),
        Some("concurrency_conflict") => Err(Error::conflict(
            expected_edit_version,
            row.observed_edit_version.ok_or_else(Error::internal)?,
        )),
        _ => Err(Error::internal()),
    }
}

/// `widget.delete`: delete one row at the revision the caller last read.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
pub async fn delete(
    transaction: &mut wamn_postgres_statements::Transaction,
    id: &str,
    expected_edit_version: i64,
) -> Result<WidgetDeleteRow, Error> {
    let id = scalar::uuid(id).map_err(|Invalid| Error::invalid("id"))?;
    let row = sql::delete(transaction, id.clone(), expected_edit_version)
        .await
        .map_err(|error| {
            Error::from_statement(
                &error,
                &Constraints {
                    unique: sql::DELETE_UNIQUE_CONSTRAINTS,
                    foreign_key: sql::DELETE_FOREIGN_KEY_CONSTRAINTS,
                    check: sql::DELETE_CHECK_CONSTRAINTS,
                    exclusion: sql::DELETE_EXCLUSION_CONSTRAINTS,
                },
                DELETE_OPERATION,
            )
        })?;
    match row.outcome.as_deref() {
        Some("deleted") => Ok(row),
        Some("not_found") => Err(Error::not_found(&id)),
        Some("concurrency_conflict") => Err(Error::conflict(
            expected_edit_version,
            row.observed_edit_version.ok_or_else(Error::internal)?,
        )),
        _ => Err(Error::internal()),
    }
}

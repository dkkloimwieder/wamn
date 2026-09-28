// @generated from the package manifest and migration IR; do not edit.

// The generated `packaging` operations.

#[allow(unused_imports)]
use wamn_data_access::{Direction, Invalid, Page, cursor, scalar};
use wamn_postgres_statements::Connection;

#[allow(unused_imports)]
use super::error::{Constraints, Error};

/// The statement accessors of the model.
pub mod sql {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wamn/packaging.rs"
    ));
}

pub use sql::PackagingRow;

const GET_OPERATION: &str = "packaging.get";
const QUERY_OPERATION: &str = "packaging.query";
const CREATE_OPERATION: &str = "packaging.create";

/// `packaging.get`: load one row by id.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
pub async fn get(connection: &mut Connection, id: &str) -> Result<PackagingRow, Error> {
    let id = scalar::uuid(id).map_err(|Invalid| Error::invalid("id"))?;
    sql::get(connection, id.clone())
        .await
        .map_err(|error| Error::from_statement(&error, &Constraints::NONE, GET_OPERATION))?
        .ok_or_else(|| Error::not_found(&id))
}

/// The `packaging.query` request, after its codec filled the limit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryInput {
    pub status: Option<Vec<String>>,
    pub location_id: Option<Vec<String>>,
    pub packaging_code: Option<Vec<String>>,
    pub sort_field: Option<String>,
    pub sort_direction: Option<String>,
    pub cursor: Option<String>,
    pub limit: i64,
}

fn query_statement(error: &wamn_postgres_statements::StatementError) -> Error {
    Error::from_statement(error, &Constraints::NONE, QUERY_OPERATION)
}

/// `packaging.query`: one keyset page in the requested sort.
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
) -> Result<Page<PackagingRow, Error>, Error> {
    let sort = match (input.sort_field.as_deref(), input.sort_direction.as_deref()) {
        (None, None) => ("created_at", Direction::Ascending),
        (Some(field), Some("ascending")) => (field, Direction::Ascending),
        (Some(field), Some("descending")) => (field, Direction::Descending),
        _ => return Err(Error::invalid("sort")),
    };
    let status_filter = input.status.as_deref().map(scalar::json_list);
    let location_id_filter = input.location_id.as_deref().map(scalar::json_list);
    let packaging_code_filter = input.packaging_code.as_deref().map(scalar::json_list);
    match sort {
        ("packaging_code", Direction::Ascending) => {
            let (cursor_key, cursor_id) = match input.cursor.as_deref() {
                Some(encoded) => {
                    let (key, id) =
                        cursor::decode::<String>(encoded, "packaging_code", Direction::Ascending)
                            .map_err(|Invalid| Error::invalid("cursor"))?;
                    (Some(key), Some(id))
                }
                None => (None, None),
            };
            let rows = sql::query_packaging_code_ascending(
                connection,
                status_filter,
                location_id_filter,
                packaging_code_filter,
                cursor_key,
                cursor_id,
                input.limit + 1,
            )
            .await
            .map_err(|error| query_statement(&error))?;
            Ok(Page::new(rows, input.limit, query_statement, |row| {
                cursor::encode(
                    "packaging_code",
                    Direction::Ascending,
                    &row.packaging_code,
                    &row.id,
                )
                .map_err(|Invalid| Error::internal())
            }))
        }
        ("packaging_code", Direction::Descending) => {
            let (cursor_key, cursor_id) = match input.cursor.as_deref() {
                Some(encoded) => {
                    let (key, id) =
                        cursor::decode::<String>(encoded, "packaging_code", Direction::Descending)
                            .map_err(|Invalid| Error::invalid("cursor"))?;
                    (Some(key), Some(id))
                }
                None => (None, None),
            };
            let rows = sql::query_packaging_code_descending(
                connection,
                status_filter,
                location_id_filter,
                packaging_code_filter,
                cursor_key,
                cursor_id,
                input.limit + 1,
            )
            .await
            .map_err(|error| query_statement(&error))?;
            Ok(Page::new(rows, input.limit, query_statement, |row| {
                cursor::encode(
                    "packaging_code",
                    Direction::Descending,
                    &row.packaging_code,
                    &row.id,
                )
                .map_err(|Invalid| Error::internal())
            }))
        }
        ("location_id", Direction::Ascending) => {
            let (cursor_key, cursor_id) = match input.cursor.as_deref() {
                Some(encoded) => {
                    let (key, id) = cursor::decode::<wamn_postgres_statements::Uuid>(
                        encoded,
                        "location_id",
                        Direction::Ascending,
                    )
                    .map_err(|Invalid| Error::invalid("cursor"))?;
                    (Some(key), Some(id))
                }
                None => (None, None),
            };
            let rows = sql::query_location_id_ascending(
                connection,
                status_filter,
                location_id_filter,
                packaging_code_filter,
                cursor_key,
                cursor_id,
                input.limit + 1,
            )
            .await
            .map_err(|error| query_statement(&error))?;
            Ok(Page::new(rows, input.limit, query_statement, |row| {
                cursor::encode(
                    "location_id",
                    Direction::Ascending,
                    &row.location_id,
                    &row.id,
                )
                .map_err(|Invalid| Error::internal())
            }))
        }
        ("location_id", Direction::Descending) => {
            let (cursor_key, cursor_id) = match input.cursor.as_deref() {
                Some(encoded) => {
                    let (key, id) = cursor::decode::<wamn_postgres_statements::Uuid>(
                        encoded,
                        "location_id",
                        Direction::Descending,
                    )
                    .map_err(|Invalid| Error::invalid("cursor"))?;
                    (Some(key), Some(id))
                }
                None => (None, None),
            };
            let rows = sql::query_location_id_descending(
                connection,
                status_filter,
                location_id_filter,
                packaging_code_filter,
                cursor_key,
                cursor_id,
                input.limit + 1,
            )
            .await
            .map_err(|error| query_statement(&error))?;
            Ok(Page::new(rows, input.limit, query_statement, |row| {
                cursor::encode(
                    "location_id",
                    Direction::Descending,
                    &row.location_id,
                    &row.id,
                )
                .map_err(|Invalid| Error::internal())
            }))
        }
        ("updated_at", Direction::Ascending) => {
            let (cursor_key, cursor_id) = match input.cursor.as_deref() {
                Some(encoded) => {
                    let (key, id) = cursor::decode::<wamn_postgres_statements::TimestampTz>(
                        encoded,
                        "updated_at",
                        Direction::Ascending,
                    )
                    .map_err(|Invalid| Error::invalid("cursor"))?;
                    (Some(key), Some(id))
                }
                None => (None, None),
            };
            let rows = sql::query_updated_at_ascending(
                connection,
                status_filter,
                location_id_filter,
                packaging_code_filter,
                cursor_key,
                cursor_id,
                input.limit + 1,
            )
            .await
            .map_err(|error| query_statement(&error))?;
            Ok(Page::new(rows, input.limit, query_statement, |row| {
                cursor::encode("updated_at", Direction::Ascending, &row.updated_at, &row.id)
                    .map_err(|Invalid| Error::internal())
            }))
        }
        ("updated_at", Direction::Descending) => {
            let (cursor_key, cursor_id) = match input.cursor.as_deref() {
                Some(encoded) => {
                    let (key, id) = cursor::decode::<wamn_postgres_statements::TimestampTz>(
                        encoded,
                        "updated_at",
                        Direction::Descending,
                    )
                    .map_err(|Invalid| Error::invalid("cursor"))?;
                    (Some(key), Some(id))
                }
                None => (None, None),
            };
            let rows = sql::query_updated_at_descending(
                connection,
                status_filter,
                location_id_filter,
                packaging_code_filter,
                cursor_key,
                cursor_id,
                input.limit + 1,
            )
            .await
            .map_err(|error| query_statement(&error))?;
            Ok(Page::new(rows, input.limit, query_statement, |row| {
                cursor::encode(
                    "updated_at",
                    Direction::Descending,
                    &row.updated_at,
                    &row.id,
                )
                .map_err(|Invalid| Error::internal())
            }))
        }
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
                status_filter,
                location_id_filter,
                packaging_code_filter,
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
                status_filter,
                location_id_filter,
                packaging_code_filter,
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

/// `packaging.create`: create one row in the transaction the codec holds
/// for the write log.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    transaction: &mut wamn_postgres_statements::Transaction,
    packaging_code: Option<String>,
    r#type: Option<String>,
    location_id: Option<String>,
    status: Option<String>,
) -> Result<PackagingRow, Error> {
    let Some(value) = packaging_code else {
        return Err(Error::invalid("packaging_code"));
    };
    let packaging_code = value;
    let Some(value) = r#type else {
        return Err(Error::invalid("type"));
    };
    let r#type = value;
    let Some(value) = location_id else {
        return Err(Error::invalid("location_id"));
    };
    let location_id = scalar::uuid(&value).map_err(|Invalid| Error::invalid("location_id"))?;
    let Some(value) = status else {
        return Err(Error::invalid("status"));
    };
    let status = value;
    sql::create(transaction, packaging_code, r#type, location_id, status)
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

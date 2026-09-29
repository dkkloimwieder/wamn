// @generated from the package manifest and migration IR; do not edit.

// The generated `purchase_order` operations.

#[allow(unused_imports)]
use wamn_data_access::{Direction, Invalid, Page, cursor, scalar};
use wamn_postgres_statements::Connection;

#[allow(unused_imports)]
use super::error::{Constraints, Error};

/// The statement accessors of the model.
pub mod sql {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wamn/purchase_order.rs"
    ));
}

pub use sql::PurchaseOrderRow;
pub use sql::PurchaseOrderUpdateRow;

const GET_OPERATION: &str = "purchase_order.get";
const QUERY_OPERATION: &str = "purchase_order.query";
const UPDATE_OPERATION: &str = "purchase_order.update";

/// `purchase_order.get`: load one row by id.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
pub async fn get(connection: &mut Connection, id: &str) -> Result<PurchaseOrderRow, Error> {
    let id = scalar::uuid(id).map_err(|Invalid| Error::invalid("id"))?;
    sql::get(connection, id.clone())
        .await
        .map_err(|error| Error::from_statement(&error, &Constraints::NONE, GET_OPERATION))?
        .ok_or_else(|| Error::not_found(&id))
}

/// The `purchase_order.query` request, after its codec filled the limit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryInput {
    pub supplier_id: Option<Vec<String>>,
    pub status: Option<Vec<String>>,
    pub purchase_order_number: Option<Vec<String>>,
    pub sort_field: Option<String>,
    pub sort_direction: Option<String>,
    pub cursor: Option<String>,
    pub limit: i64,
}

fn query_statement(error: &wamn_postgres_statements::StatementError) -> Error {
    Error::from_statement(error, &Constraints::NONE, QUERY_OPERATION)
}

/// `purchase_order.query`: one keyset page in the requested sort.
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
) -> Result<Page<PurchaseOrderRow, Error>, Error> {
    let sort = match (input.sort_field.as_deref(), input.sort_direction.as_deref()) {
        (None, None) => ("created_at", Direction::Ascending),
        (Some(field), Some("ascending")) => (field, Direction::Ascending),
        (Some(field), Some("descending")) => (field, Direction::Descending),
        _ => return Err(Error::invalid("sort")),
    };
    let supplier_id_filter = input.supplier_id.as_deref().map(scalar::json_list);
    let status_filter = input.status.as_deref().map(scalar::json_list);
    let purchase_order_number_filter = input
        .purchase_order_number
        .as_deref()
        .map(scalar::json_list);
    match sort {
        ("purchase_order_number", Direction::Ascending) => {
            let (cursor_key, cursor_id) = match input.cursor.as_deref() {
                Some(encoded) => {
                    let (key, id) = cursor::decode::<String>(
                        encoded,
                        "purchase_order_number",
                        Direction::Ascending,
                    )
                    .map_err(|Invalid| Error::invalid("cursor"))?;
                    (Some(key), Some(id))
                }
                None => (None, None),
            };
            let rows = sql::query_purchase_order_number_ascending(
                connection,
                supplier_id_filter,
                status_filter,
                purchase_order_number_filter,
                cursor_key,
                cursor_id,
                input.limit + 1,
            )
            .await
            .map_err(|error| query_statement(&error))?;
            Ok(Page::new(rows, input.limit, query_statement, |row| {
                cursor::encode(
                    "purchase_order_number",
                    Direction::Ascending,
                    &row.purchase_order_number,
                    &row.id,
                )
                .map_err(|Invalid| Error::internal())
            }))
        }
        ("purchase_order_number", Direction::Descending) => {
            let (cursor_key, cursor_id) = match input.cursor.as_deref() {
                Some(encoded) => {
                    let (key, id) = cursor::decode::<String>(
                        encoded,
                        "purchase_order_number",
                        Direction::Descending,
                    )
                    .map_err(|Invalid| Error::invalid("cursor"))?;
                    (Some(key), Some(id))
                }
                None => (None, None),
            };
            let rows = sql::query_purchase_order_number_descending(
                connection,
                supplier_id_filter,
                status_filter,
                purchase_order_number_filter,
                cursor_key,
                cursor_id,
                input.limit + 1,
            )
            .await
            .map_err(|error| query_statement(&error))?;
            Ok(Page::new(rows, input.limit, query_statement, |row| {
                cursor::encode(
                    "purchase_order_number",
                    Direction::Descending,
                    &row.purchase_order_number,
                    &row.id,
                )
                .map_err(|Invalid| Error::internal())
            }))
        }
        ("status", Direction::Ascending) => {
            let (cursor_key, cursor_id) = match input.cursor.as_deref() {
                Some(encoded) => {
                    let (key, id) =
                        cursor::decode::<String>(encoded, "status", Direction::Ascending)
                            .map_err(|Invalid| Error::invalid("cursor"))?;
                    (Some(key), Some(id))
                }
                None => (None, None),
            };
            let rows = sql::query_status_ascending(
                connection,
                supplier_id_filter,
                status_filter,
                purchase_order_number_filter,
                cursor_key,
                cursor_id,
                input.limit + 1,
            )
            .await
            .map_err(|error| query_statement(&error))?;
            Ok(Page::new(rows, input.limit, query_statement, |row| {
                cursor::encode("status", Direction::Ascending, &row.status, &row.id)
                    .map_err(|Invalid| Error::internal())
            }))
        }
        ("status", Direction::Descending) => {
            let (cursor_key, cursor_id) = match input.cursor.as_deref() {
                Some(encoded) => {
                    let (key, id) =
                        cursor::decode::<String>(encoded, "status", Direction::Descending)
                            .map_err(|Invalid| Error::invalid("cursor"))?;
                    (Some(key), Some(id))
                }
                None => (None, None),
            };
            let rows = sql::query_status_descending(
                connection,
                supplier_id_filter,
                status_filter,
                purchase_order_number_filter,
                cursor_key,
                cursor_id,
                input.limit + 1,
            )
            .await
            .map_err(|error| query_statement(&error))?;
            Ok(Page::new(rows, input.limit, query_statement, |row| {
                cursor::encode("status", Direction::Descending, &row.status, &row.id)
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
                supplier_id_filter,
                status_filter,
                purchase_order_number_filter,
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
                supplier_id_filter,
                status_filter,
                purchase_order_number_filter,
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

/// `purchase_order.update`: change one row at the revision the caller last read.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
#[allow(clippy::too_many_arguments)]
pub async fn update(
    transaction: &mut wamn_postgres_statements::Transaction,
    id: &str,
    expected_row_version: i32,
    supplier_id: Option<Option<String>>,
) -> Result<PurchaseOrderRow, Error> {
    let id = scalar::uuid(id).map_err(|Invalid| Error::invalid("id"))?;
    if matches!(supplier_id, Some(None)) {
        return Err(Error::invalid("change.supplier_id"));
    }
    let supplier_id_present = supplier_id.is_some();
    let supplier_id = match supplier_id.flatten() {
        Some(value) => {
            Some(scalar::uuid(&value).map_err(|Invalid| Error::invalid("change.supplier_id"))?)
        }
        None => None,
    };
    let row = sql::update(
        transaction,
        id.clone(),
        expected_row_version,
        supplier_id_present,
        supplier_id,
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
        Some("updated") => Ok(PurchaseOrderRow {
            created_at: row.created_at.ok_or_else(Error::internal)?,
            created_by: row.created_by.ok_or_else(Error::internal)?,
            id: row.id.ok_or_else(Error::internal)?,
            purchase_order_number: row.purchase_order_number.ok_or_else(Error::internal)?,
            row_version: row.row_version.ok_or_else(Error::internal)?,
            status: row.status.ok_or_else(Error::internal)?,
            supplier_id: row.supplier_id.ok_or_else(Error::internal)?,
            updated_at: row.updated_at.ok_or_else(Error::internal)?,
            updated_by: row.updated_by.ok_or_else(Error::internal)?,
        }),
        Some("not_found") => Err(Error::not_found(&id)),
        Some("concurrency_conflict") => Err(Error::conflict(
            i64::from(expected_row_version),
            i64::from(row.observed_row_version.ok_or_else(Error::internal)?),
        )),
        _ => Err(Error::internal()),
    }
}

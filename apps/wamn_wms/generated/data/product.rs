// @generated from the package manifest and migration IR; do not edit.

// The generated `product` operations.

#[allow(unused_imports)]
use wamn_data_access::{Direction, Invalid, Page, cursor, scalar};
use wamn_postgres_statements::Connection;

#[allow(unused_imports)]
use super::error::{Constraints, Error};

/// The statement accessors of the model.
pub mod sql {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wamn/product.rs"
    ));
}

pub use sql::ProductRow;
pub use sql::ProductUpdateRow;

const GET_OPERATION: &str = "product.get";
const QUERY_OPERATION: &str = "product.query";
const CREATE_OPERATION: &str = "product.create";
const UPDATE_OPERATION: &str = "product.update";

/// `product.get`: load one row by id.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
pub async fn get(connection: &mut Connection, id: &str) -> Result<ProductRow, Error> {
    let id = scalar::uuid(id).map_err(|Invalid| Error::invalid("id"))?;
    sql::get(connection, id.clone())
        .await
        .map_err(|error| Error::from_statement(&error, &Constraints::NONE, GET_OPERATION))?
        .ok_or_else(|| Error::not_found(&id))
}

/// The `product.query` request, after its codec filled the limit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryInput {
    pub product_code: Option<Vec<String>>,
    pub sort_field: Option<String>,
    pub sort_direction: Option<String>,
    pub cursor: Option<String>,
    pub limit: i64,
}

fn query_statement(error: &wamn_postgres_statements::StatementError) -> Error {
    Error::from_statement(error, &Constraints::NONE, QUERY_OPERATION)
}

/// `product.query`: one keyset page in the requested sort.
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
) -> Result<Page<ProductRow, Error>, Error> {
    let sort = match (input.sort_field.as_deref(), input.sort_direction.as_deref()) {
        (None, None) => ("created_at", Direction::Ascending),
        (Some(field), Some("ascending")) => (field, Direction::Ascending),
        (Some(field), Some("descending")) => (field, Direction::Descending),
        _ => return Err(Error::invalid("sort")),
    };
    let product_code_filter = input.product_code.as_deref().map(scalar::json_list);
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
                product_code_filter,
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

        _ => Err(Error::invalid("sort")),
    }
}

/// `product.create`: create one row in the transaction the codec holds
/// for the write log.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    transaction: &mut wamn_postgres_statements::Transaction,
    product_code: Option<String>,
) -> Result<ProductRow, Error> {
    let Some(value) = product_code else {
        return Err(Error::invalid("product_code"));
    };
    let product_code = value;
    sql::create(transaction, product_code)
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

/// `product.update`: change one row at the revision the caller last read.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
#[allow(clippy::too_many_arguments)]
pub async fn update(
    connection: &mut Connection,
    id: &str,
    expected_row_version: i32,
    product_code: Option<Option<String>>,
) -> Result<ProductRow, Error> {
    let id = scalar::uuid(id).map_err(|Invalid| Error::invalid("id"))?;
    if matches!(product_code, Some(None)) {
        return Err(Error::invalid("change.product_code"));
    }
    let product_code_present = product_code.is_some();
    let product_code = product_code.flatten();
    let row = sql::update(
        connection,
        id.clone(),
        expected_row_version,
        product_code_present,
        product_code,
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
        Some("updated") => Ok(ProductRow {
            created_at: row.created_at.ok_or_else(Error::internal)?,
            id: row.id.ok_or_else(Error::internal)?,
            product_code: row.product_code.ok_or_else(Error::internal)?,
            row_version: row.row_version.ok_or_else(Error::internal)?,
        }),
        Some("not_found") => Err(Error::not_found(&id)),
        Some("concurrency_conflict") => Err(Error::conflict(
            i64::from(expected_row_version),
            i64::from(row.observed_row_version.ok_or_else(Error::internal)?),
        )),
        _ => Err(Error::internal()),
    }
}

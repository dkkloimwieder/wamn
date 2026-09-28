// @generated from the package manifest and migration IR; do not edit.

// The generated `supplier` operations.

#[allow(unused_imports)]
use wamn_data_access::{Direction, Invalid, Page, cursor, scalar};
use wamn_postgres_statements::Connection;

#[allow(unused_imports)]
use super::error::{Constraints, Error};

/// The statement accessors of the model.
pub mod sql {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wamn/supplier.rs"
    ));
}

pub use sql::SupplierRow;

const QUERY_OPERATION: &str = "supplier.query";
const CREATE_OPERATION: &str = "supplier.create";

/// The `supplier.query` request, after its codec filled the limit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryInput {
    pub sort_field: Option<String>,
    pub sort_direction: Option<String>,
    pub cursor: Option<String>,
    pub limit: i64,
}

fn query_statement(error: &wamn_postgres_statements::StatementError) -> Error {
    Error::from_statement(error, &Constraints::NONE, QUERY_OPERATION)
}

/// `supplier.query`: one keyset page in the requested sort.
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
) -> Result<Page<SupplierRow, Error>, Error> {
    let sort = match (input.sort_field.as_deref(), input.sort_direction.as_deref()) {
        (None, None) => ("created_at", Direction::Ascending),
        (Some(field), Some("ascending")) => (field, Direction::Ascending),
        (Some(field), Some("descending")) => (field, Direction::Descending),
        _ => return Err(Error::invalid("sort")),
    };
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
            let rows =
                sql::query_created_at_ascending(connection, cursor_key, cursor_id, input.limit + 1)
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

/// `supplier.create`: create one row in the transaction the codec holds
/// for the write log.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    transaction: &mut wamn_postgres_statements::Transaction,
    name: Option<String>,
) -> Result<SupplierRow, Error> {
    let Some(value) = name else {
        return Err(Error::invalid("name"));
    };
    let name = value;
    sql::create(transaction, name).await.map_err(|error| {
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

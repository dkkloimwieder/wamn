// @generated from the package manifest and migration IR; do not edit.

// The generated `widget_maker` operations.

#[allow(unused_imports)]
use wamn_data_access::{Direction, Invalid, Page, cursor, scalar};
use wamn_postgres_statements::Connection;

#[allow(unused_imports)]
use super::error::{Constraints, Error};

/// The statement accessors of the model.
pub mod sql {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wamn/widget_maker.rs"
    ));
}

pub use sql::WidgetMakerRow;

const GET_OPERATION: &str = "widget_maker.get";
const QUERY_OPERATION: &str = "widget_maker.query";

/// `widget_maker.get`: load one row by id.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
pub async fn get(connection: &mut Connection, id: &str) -> Result<WidgetMakerRow, Error> {
    let id = scalar::uuid(id).map_err(|Invalid| Error::invalid("id"))?;
    sql::get(connection, id.clone())
        .await
        .map_err(|error| Error::from_statement(&error, &Constraints::NONE, GET_OPERATION))?
        .ok_or_else(|| Error::not_found(&id))
}

pub use wamn_data_access::Range;

/// The `widget_maker.query` request, after its codec filled the limit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryInput {
    pub name: Option<Vec<String>>,
    pub created_at: Option<wamn_data_access::Range>,
    pub search: Option<String>,
    pub sort_field: Option<String>,
    pub sort_direction: Option<String>,
    pub cursor: Option<String>,
    pub limit: i64,
}

fn query_statement(error: &wamn_postgres_statements::StatementError) -> Error {
    Error::from_statement(error, &Constraints::NONE, QUERY_OPERATION)
}

/// `widget_maker.query`: one keyset page in the requested sort.
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
) -> Result<Page<WidgetMakerRow, Error>, Error> {
    let sort = match (input.sort_field.as_deref(), input.sort_direction.as_deref()) {
        (None, None) => ("created_at", Direction::Ascending),
        (Some(field), Some("ascending")) => (field, Direction::Ascending),
        (Some(field), Some("descending")) => (field, Direction::Descending),
        _ => return Err(Error::invalid("sort")),
    };
    let name_filter = input.name.as_deref().map(scalar::json_list);
    let created_at_filter = input
        .created_at
        .as_ref()
        .map(|range| scalar::json_range(range.min.as_deref(), range.max.as_deref()));
    let search = input.search;
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
                name_filter,
                created_at_filter,
                search,
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
                name_filter,
                created_at_filter,
                search,
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

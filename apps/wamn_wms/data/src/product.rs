//! `product` get, query, create and update over the generated statements.

use wamn_postgres_statements::{Connection, StatementError, Transaction};

use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::product as sql;
use crate::page::{self, Page};
use crate::scalar;

pub use crate::generated::wamn::product::ProductRow;

/// What an update does to `product_code`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodeChange {
    Omitted,
    Null,
    Value(String),
}

/// Load one product by id.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn get(connection: &mut Connection, id: &str) -> Result<ProductRow, AccessError> {
    let id = scalar::uuid("id", id)?;
    sql::get(connection, id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(|| AccessError::missing(AccessErrorKind::NotFound, "id", &id.0))
}

/// Query one read, narrowed to the codes named when any are.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn query(
    connection: &mut Connection,
    product_codes: Option<&[String]>,
    cursor: Option<&str>,
    limit: i64,
) -> Result<Page<ProductRow>, AccessError> {
    let (key, id) = page::start(cursor)?;
    let codes = product_codes.map(page::text_json);
    let rows = sql::query_created_at_ascending(connection, codes, key, id, limit + 1)
        .await
        .map_err(|e| error::from_statement(&e))?;
    Ok(Page::new(rows, limit, |row| {
        page::created_at_cursor(&row.created_at, &row.id)
    }))
}

/// Create one product in the transaction its codec holds for the write log.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn create(
    transaction: &mut Transaction,
    product_code: Option<&str>,
) -> Result<ProductRow, AccessError> {
    let product_code = scalar::text("product_code", product_code)?;
    sql::create(transaction, product_code.to_owned())
        .await
        .map_err(|e| refuse(&e))
}

/// Change one product at the revision the caller read.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn update(
    connection: &mut Connection,
    id: &str,
    expected_row_version: i32,
    product_code: CodeChange,
) -> Result<ProductRow, AccessError> {
    let id = scalar::uuid("id", id)?;
    let (present, value) = match product_code {
        CodeChange::Omitted => (false, None),
        CodeChange::Null => {
            return Err(AccessError::field(
                AccessErrorKind::InvalidInput,
                "change.product_code",
            ));
        }
        CodeChange::Value(value) => (
            true,
            Some(scalar::text("change.product_code", Some(&value))?.to_owned()),
        ),
    };
    let row = sql::update(connection, id.clone(), expected_row_version, present, value)
        .await
        .map_err(|e| {
            error::from_write(
                &e,
                sql::UPDATE_UNIQUE_CONSTRAINTS,
                sql::UPDATE_FOREIGN_KEY_CONSTRAINTS,
                sql::UPDATE_CHECK_CONSTRAINTS,
            )
        })?;
    match (row.outcome.as_deref(), row.observed_row_version) {
        (Some("not_found"), _) => Err(AccessError::missing(AccessErrorKind::NotFound, "id", &id.0)),
        (Some("concurrency_conflict"), Some(observed)) => {
            Err(AccessError::conflict(expected_row_version, observed))
        }
        (Some("updated"), _) => match (row.created_at, row.id, row.product_code, row.row_version) {
            (Some(created_at), Some(id), Some(product_code), Some(row_version)) => Ok(ProductRow {
                created_at,
                id,
                product_code,
                row_version,
            }),
            _ => Err(AccessError::new(
                AccessErrorKind::InternalError,
                serde_json::json!({}),
            )),
        },
        _ => Err(AccessError::new(
            AccessErrorKind::InternalError,
            serde_json::json!({}),
        )),
    }
}

fn refuse(error: &StatementError) -> AccessError {
    error::from_write(
        error,
        sql::CREATE_UNIQUE_CONSTRAINTS,
        sql::CREATE_FOREIGN_KEY_CONSTRAINTS,
        sql::CREATE_CHECK_CONSTRAINTS,
    )
}

//! `product` get, query, create and update over the generated statements.

use wamn_postgres_statements::{Connection, StatementError};

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

/// Query one bounded page, narrowed to the codes named when any are.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn query(
    connection: &mut Connection,
    product_codes: Option<&[String]>,
    cursor: Option<&str>,
    limit: Option<i64>,
) -> Result<Page<ProductRow>, AccessError> {
    let ((key, id), limit) = page::start(cursor, limit)?;
    let codes = product_codes.map(page::text_json);
    let rows = sql::query_created_at_ascending(connection, codes, key, id, limit + 1)
        .await
        .map_err(|e| error::from_statement(&e))?;
    page::finish(rows, limit, |row| (&row.created_at, &row.id))
}

/// Create one product under the identity its claim row mints.
///
/// A retry of the same key returns the row the first attempt wrote, and the
/// same key under a different code refuses.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn create(
    connection: &mut Connection,
    idempotency_key: &str,
    product_code: Option<&str>,
) -> Result<ProductRow, AccessError> {
    let product_code = scalar::text("product_code", product_code)?;
    let canonical = wamn_execution_contract::canonical_json_bytes(
        &serde_json::json!({ "product_code": product_code }),
    );
    let transaction = connection.begin().await.map_err(|e| refuse(&e))?;
    let mut claim = sql::begin_claim(transaction);
    let replay = sql::create_replay(&mut claim, idempotency_key.to_owned())
        .await
        .map_err(|e| refuse(&e))?;
    if let Some(replay) = replay {
        return replay_row(replay, &canonical);
    }
    let claimed = sql::create_claim(&mut claim, idempotency_key.to_owned(), canonical.clone())
        .await
        .map_err(|e| refuse(&e))?;
    let Some(claimed) = claimed else {
        // Another caller took the key between the replay read and this insert.
        // Its row is committed, so the replay read now answers.
        let replay = sql::create_replay(&mut claim, idempotency_key.to_owned())
            .await
            .map_err(|e| refuse(&e))?
            .ok_or_else(|| AccessError::new(AccessErrorKind::Retry, serde_json::json!({})))?;
        return replay_row(replay, &canonical);
    };
    let finalized = sql::create(claim, claimed.product_id, product_code.to_owned())
        .await
        .map_err(|e| refuse(&e))?;
    let row = ProductRow {
        created_at: finalized.row.created_at.clone(),
        id: finalized.row.id.clone(),
        product_code: finalized.row.product_code.clone(),
        row_version: finalized.row.row_version,
    };
    finalized.commit().await.map_err(|e| refuse(&e))?;
    Ok(row)
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
        (Some("concurrency_conflict"), Some(observed)) => Err(AccessError::conflict(
            i64::from(expected_row_version),
            i64::from(observed),
        )),
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

fn replay_row(
    replay: sql::ProductCreateReplayRow,
    canonical: &[u8],
) -> Result<ProductRow, AccessError> {
    if replay.canonical_command != canonical {
        return Err(AccessError::field(
            AccessErrorKind::IdempotencyConflict,
            "idempotency_key",
        ));
    }
    Ok(ProductRow {
        created_at: replay.created_at,
        id: replay.id,
        product_code: replay.product_code,
        row_version: replay.row_version,
    })
}

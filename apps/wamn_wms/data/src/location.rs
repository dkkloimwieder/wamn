//! `location` get, query, create and update over the generated statements.

use wamn_postgres_statements::{Connection, StatementError};

use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::location as sql;
use crate::page::{self, Page};
use crate::scalar;

pub use crate::generated::wamn::location::LocationRow;

/// What an update does to `location_code`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodeChange {
    Omitted,
    Null,
    Value(String),
}

/// Load one location by id.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn get(connection: &mut Connection, id: &str) -> Result<LocationRow, AccessError> {
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
    location_codes: Option<&[String]>,
    cursor: Option<&str>,
    limit: Option<i64>,
) -> Result<Page<LocationRow>, AccessError> {
    let ((key, id), limit) = page::start(cursor, limit)?;
    let codes = location_codes.map(page::text_json);
    let rows = sql::query_created_at_ascending(connection, codes, key, id, limit + 1)
        .await
        .map_err(|e| error::from_statement(&e))?;
    page::finish(rows, limit, |row| (&row.created_at, &row.id))
}

/// Create one location under the identity its claim row mints.
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
    location_code: Option<&str>,
) -> Result<LocationRow, AccessError> {
    let location_code = scalar::text("location_code", location_code)?;
    let canonical = wamn_execution_contract::canonical_json_bytes(
        &serde_json::json!({ "location_code": location_code }),
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
    let finalized = sql::create(claim, claimed.location_id, location_code.to_owned())
        .await
        .map_err(|e| refuse(&e))?;
    let row = LocationRow {
        created_at: finalized.row.created_at.clone(),
        id: finalized.row.id.clone(),
        location_code: finalized.row.location_code.clone(),
        row_version: finalized.row.row_version,
    };
    finalized.commit().await.map_err(|e| refuse(&e))?;
    Ok(row)
}

/// Change one location at the revision the caller read.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn update(
    connection: &mut Connection,
    id: &str,
    expected_row_version: i32,
    location_code: CodeChange,
) -> Result<LocationRow, AccessError> {
    let id = scalar::uuid("id", id)?;
    let (present, value) = match location_code {
        CodeChange::Omitted => (false, None),
        CodeChange::Null => {
            return Err(AccessError::field(
                AccessErrorKind::InvalidInput,
                "change.location_code",
            ));
        }
        CodeChange::Value(value) => (
            true,
            Some(scalar::text("change.location_code", Some(&value))?.to_owned()),
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
        (Some("updated"), _) => {
            match (row.created_at, row.id, row.location_code, row.row_version) {
                (Some(created_at), Some(id), Some(location_code), Some(row_version)) => {
                    Ok(LocationRow {
                        created_at,
                        id,
                        location_code,
                        row_version,
                    })
                }
                _ => Err(AccessError::new(
                    AccessErrorKind::InternalError,
                    serde_json::json!({}),
                )),
            }
        }
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
    replay: sql::LocationCreateReplayRow,
    canonical: &[u8],
) -> Result<LocationRow, AccessError> {
    if replay.canonical_command != canonical {
        return Err(AccessError::field(
            AccessErrorKind::IdempotencyConflict,
            "idempotency_key",
        ));
    }
    Ok(LocationRow {
        created_at: replay.created_at,
        id: replay.id,
        location_code: replay.location_code,
        row_version: replay.row_version,
    })
}

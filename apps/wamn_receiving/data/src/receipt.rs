//! Runtime-checked `receipt` read operations.

use chrono::{DateTime, SecondsFormat, Utc};
use uuid::Uuid;
use wamn_postgres_statements::{Connection, TimestampTz, Uuid as WamnUuid};

use crate::cursor::{CursorDirection, decode_cursor, encode_cursor};
use crate::error::{AccessError, AllowedConstraints};
use crate::generated::wamn::receipt as generated;
use crate::page::Page;

#[doc(inline)]
pub use crate::generated::wamn::receipt::ReceiptRow;

/// Typed receipt query input.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryInput {
    pub cursor: Option<Box<str>>,
    pub limit: i64,
}

/// Load one immutable receipt by UUID, in any accepted spelling.
pub async fn get(connection: &mut Connection, id: &str) -> Result<ReceiptRow, AccessError> {
    let id = parse_input_uuid(id, "receipt id")?;
    generated::get(connection, WamnUuid(id.hyphenated().to_string()))
        .await
        .map_err(|source| {
            AccessError::from_statement("load receipt", &source, AllowedConstraints::NONE)
        })?
        .ok_or_else(|| AccessError::not_found("receipt does not exist"))
}

/// Query one read in generated `created_at, id` order.
pub async fn query(
    connection: &mut Connection,
    input: &QueryInput,
) -> Result<Page<ReceiptRow>, AccessError> {
    let cursor = input
        .cursor
        .as_deref()
        .map(|cursor| {
            decode_cursor::<DateTime<Utc>>(cursor, "created_at", CursorDirection::Ascending)
        })
        .transpose()?;
    // The operation's codec checked the limit.
    let limit = input.limit;
    let (cursor_created_at, cursor_id) = match cursor {
        Some(cursor) => (
            Some(TimestampTz(
                cursor.key.to_rfc3339_opts(SecondsFormat::Micros, true),
            )),
            Some(WamnUuid(cursor.id.hyphenated().to_string())),
        ),
        None => (None, None),
    };
    let rows =
        generated::query_created_at_ascending(connection, cursor_created_at, cursor_id, limit + 1)
            .await
            .map_err(|source| {
                AccessError::from_statement("query receipt", &source, AllowedConstraints::NONE)
            })?;
    Ok(Page::new(rows, "query receipt", limit, cursor_from_row))
}

fn cursor_from_row(row: &ReceiptRow) -> Result<Box<str>, AccessError> {
    let created_at = DateTime::parse_from_rfc3339(&row.created_at.0)
        .map(|timestamp| timestamp.to_utc())
        .map_err(|_| AccessError::internal("receipt row contains an invalid created_at"))?;
    let id = parse_row_uuid(&row.id.0)?;
    encode_cursor("created_at", CursorDirection::Ascending, &created_at, id)
        .map(String::into_boxed_str)
}

/// Parse any accepted UUID spelling. The caller re-spells it
/// lowercase-hyphenated, because case is representation.
fn parse_input_uuid(value: &str, context: &str) -> Result<Uuid, AccessError> {
    Uuid::parse_str(value)
        .map_err(|_| AccessError::invalid(format!("{context} is not a UUID"), "id"))
}

/// A row arrives from PostgreSQL already canonical. A different spelling is a
/// broken invariant, not a caller choice, so this one still refuses.
fn parse_row_uuid(value: &str) -> Result<Uuid, AccessError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|parsed| parsed.hyphenated().to_string() == value)
        .ok_or_else(|| AccessError::internal("receipt row contains a noncanonical id"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::DecodedCursor;

    const FIRST_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
    const SECOND_ID: &str = "11234567-89ab-cdef-0123-456789abcdef";

    #[test]
    fn the_last_row_mints_the_cursor_that_starts_the_next_read() {
        let cursor = cursor_from_row(&row(SECOND_ID, "2026-08-29T12:01:00.123456Z")).unwrap();
        let decoded =
            decode_cursor::<DateTime<Utc>>(&cursor, "created_at", CursorDirection::Ascending)
                .unwrap();
        assert_eq!(
            decoded,
            DecodedCursor {
                key: DateTime::parse_from_rfc3339("2026-08-29T12:01:00.123456Z")
                    .unwrap()
                    .to_utc(),
                id: Uuid::parse_str(SECOND_ID).unwrap(),
            }
        );
    }

    fn row(id: &str, created_at: &str) -> ReceiptRow {
        ReceiptRow {
            created_at: TimestampTz(created_at.to_owned()),
            created_by: WamnUuid(FIRST_ID.to_owned()),
            id: WamnUuid(id.to_owned()),
            idempotency_key: "receipt-key".to_owned(),
            occurred_at: TimestampTz("2026-08-29T11:00:00.000000Z".to_owned()),
            purchase_order_id: WamnUuid(FIRST_ID.to_owned()),
            receipt_reference: "receipt-reference".to_owned(),
        }
    }
}

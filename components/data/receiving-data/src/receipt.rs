//! Runtime-checked `receipt` read operations.

use chrono::{DateTime, SecondsFormat, Utc};
use uuid::Uuid;
use wamn_postgres_sqlx::{TimestampTz, Uuid as WamnUuid, WamnConnection};

use crate::cursor::{CursorDirection, decode_cursor, encode_cursor};
use crate::error::{AccessError, AllowedConstraints};
use crate::generated::wamn::receipt as generated;

#[doc(inline)]
pub use crate::generated::wamn::receipt::ReceiptRow;

const MAX_PAGE_SIZE: i64 = 100;

/// Typed receipt query input.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryInput {
    pub cursor: Option<Box<str>>,
    pub limit: Option<i64>,
}

/// One bounded receipt page and its opaque continuation cursor.
#[derive(Debug)]
pub struct Page {
    pub item: Box<[ReceiptRow]>,
    pub next_cursor: Option<Box<str>>,
}

/// Load one immutable receipt by canonical UUID.
pub async fn get(connection: &mut WamnConnection, id: &str) -> Result<ReceiptRow, AccessError> {
    let id = parse_input_uuid(id, "receipt id")?;
    generated::get(connection, WamnUuid(id.hyphenated().to_string()))
        .await
        .map_err(|source| {
            AccessError::from_sqlx("load receipt", &source, AllowedConstraints::NONE)
        })?
        .ok_or_else(|| AccessError::not_found("receipt does not exist"))
}

/// Query one bounded page in generated `created_at, id` order.
pub async fn query(
    connection: &mut WamnConnection,
    input: &QueryInput,
) -> Result<Page, AccessError> {
    let cursor = input
        .cursor
        .as_deref()
        .map(|cursor| {
            decode_cursor::<DateTime<Utc>>(cursor, "created_at", CursorDirection::Ascending)
        })
        .transpose()?;
    let limit = validated_limit(input.limit)?;
    let page_size = usize::try_from(limit).expect("validated receipt page limit fits usize");
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
                AccessError::from_sqlx("query receipt", &source, AllowedConstraints::NONE)
            })?;
    page_from_rows(rows, page_size)
}

fn validated_limit(limit: Option<i64>) -> Result<i64, AccessError> {
    let limit = limit.unwrap_or(MAX_PAGE_SIZE);
    if (1..=MAX_PAGE_SIZE).contains(&limit) {
        Ok(limit)
    } else {
        Err(AccessError::invalid("receipt limit must be 1..=100"))
    }
}

fn page_from_rows(mut rows: Vec<ReceiptRow>, page_size: usize) -> Result<Page, AccessError> {
    let has_next = rows.len() > page_size;
    rows.truncate(page_size);
    let next_cursor = if has_next {
        rows.last().map(cursor_from_row).transpose()?
    } else {
        None
    };
    Ok(Page {
        item: rows.into_boxed_slice(),
        next_cursor,
    })
}

fn cursor_from_row(row: &ReceiptRow) -> Result<Box<str>, AccessError> {
    let created_at = DateTime::parse_from_rfc3339(&row.created_at.0)
        .map(|timestamp| timestamp.to_utc())
        .map_err(|_| AccessError::internal("receipt row contains an invalid created_at"))?;
    let id = parse_row_uuid(&row.id.0)?;
    encode_cursor("created_at", CursorDirection::Ascending, &created_at, id)
        .map(String::into_boxed_str)
}

fn parse_input_uuid(value: &str, context: &str) -> Result<Uuid, AccessError> {
    parse_canonical_uuid(value)
        .ok_or_else(|| AccessError::invalid(format!("{context} is not a canonical UUID")))
}

fn parse_row_uuid(value: &str) -> Result<Uuid, AccessError> {
    parse_canonical_uuid(value)
        .ok_or_else(|| AccessError::internal("receipt row contains a noncanonical id"))
}

fn parse_canonical_uuid(value: &str) -> Option<Uuid> {
    Uuid::parse_str(value)
        .ok()
        .filter(|parsed| parsed.hyphenated().to_string() == value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::DecodedCursor;
    use crate::error::AccessErrorKind;

    const FIRST_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
    const SECOND_ID: &str = "11234567-89ab-cdef-0123-456789abcdef";
    const THIRD_ID: &str = "21234567-89ab-cdef-0123-456789abcdef";

    #[test]
    fn limit_defaults_and_refuses_outside_the_closed_range() {
        assert_eq!(validated_limit(None).unwrap(), 100);
        assert_eq!(validated_limit(Some(1)).unwrap(), 1);
        assert_eq!(validated_limit(Some(100)).unwrap(), 100);
        for limit in [-1, 0, 101] {
            assert_eq!(
                validated_limit(Some(limit)).unwrap_err().kind(),
                AccessErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn lookahead_row_yields_cursor_from_last_returned_item() {
        let page = page_from_rows(
            vec![
                row(FIRST_ID, "2026-08-29T12:00:00.000000+00:00"),
                row(SECOND_ID, "2026-08-29T12:01:00.123456+00:00"),
                row(THIRD_ID, "2026-08-29T12:02:00.000000+00:00"),
            ],
            2,
        )
        .unwrap();

        assert_eq!(page.item.len(), 2);
        assert_eq!(page.item[1].id.0.as_str(), SECOND_ID);
        let decoded = decode_cursor::<DateTime<Utc>>(
            page.next_cursor.as_deref().unwrap(),
            "created_at",
            CursorDirection::Ascending,
        )
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

    #[test]
    fn complete_page_has_no_continuation_cursor() {
        let page =
            page_from_rows(vec![row(FIRST_ID, "2026-08-29T12:00:00.000000+00:00")], 2).unwrap();

        assert_eq!(page.item.len(), 1);
        assert!(page.next_cursor.is_none());
    }

    fn row(id: &str, created_at: &str) -> ReceiptRow {
        ReceiptRow {
            created_at: TimestampTz(created_at.to_owned()),
            id: WamnUuid(id.to_owned()),
            idempotency_key: "receipt-key".to_owned(),
            occurred_at: TimestampTz("2026-08-29T11:00:00.000000+00:00".to_owned()),
            purchase_order_id: WamnUuid(FIRST_ID.to_owned()),
            receipt_reference: "receipt-reference".to_owned(),
        }
    }
}

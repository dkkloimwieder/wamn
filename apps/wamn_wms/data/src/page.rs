//! One keyset page in the generated `created_at, id` order.
//!
//! Every model query without authored SQL pages this way: one statement, the
//! tie-breaker on `id`, and a cursor minted from the last row returned.

use chrono::{DateTime, Utc};
use serde_json::json;
use wamn_postgres_statements::{Json, TimestampTz, Uuid};

use crate::cursor::{self, CursorDirection};
use crate::error::{AccessError, AccessErrorKind};

const MAX_PAGE_SIZE: i64 = 100;
const FIELD: &str = "created_at";

/// One bounded page and the cursor that continues it, if anything does.
#[derive(Debug)]
pub struct Page<Row> {
    pub item: Vec<Row>,
    pub next_cursor: Option<String>,
}

/// The cursor's bindings: the `created_at` and the `id` of the last row.
pub(crate) type Position = (Option<TimestampTz>, Option<Uuid>);

/// Decode the cursor, then check the limit, in the contract's order.
pub(crate) fn start(
    encoded: Option<&str>,
    limit: Option<i64>,
) -> Result<(Position, i64), AccessError> {
    let position = match encoded {
        None => (None, None),
        Some(encoded) => {
            let cursor =
                cursor::decode_cursor::<DateTime<Utc>>(encoded, FIELD, CursorDirection::Ascending)?;
            (
                Some(TimestampTz(cursor::canonical_timestamp(&cursor.key))),
                Some(Uuid(cursor.id.hyphenated().to_string())),
            )
        }
    };
    let limit = limit.unwrap_or(MAX_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(AccessError::range("limit", 1, MAX_PAGE_SIZE, limit));
    }
    Ok((position, limit))
}

/// A filter's values, bound as one JSON array.
pub(crate) fn text_json(values: &[String]) -> Json {
    Json(serde_json::to_string(values).expect("strings serialize"))
}

/// Keep `limit` rows. The one row past them tells whether a next page exists.
pub(crate) fn finish<Row>(
    mut rows: Vec<Row>,
    limit: i64,
    key: impl Fn(&Row) -> (&TimestampTz, &Uuid),
) -> Result<Page<Row>, AccessError> {
    let limit = usize::try_from(limit).expect("a validated page limit fits usize");
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = match rows.last() {
        Some(last) if has_more => {
            let (created_at, id) = key(last);
            let created_at = DateTime::parse_from_rfc3339(&created_at.0)
                .map_err(|_| internal())?
                .to_utc();
            let id = uuid::Uuid::parse_str(&id.0).map_err(|_| internal())?;
            Some(cursor::encode_cursor(
                FIELD,
                CursorDirection::Ascending,
                &created_at,
                id,
            ))
        }
        _ => None,
    };
    Ok(Page {
        item: rows,
        next_cursor,
    })
}

/// A row that cannot mint a cursor is a deployment fault, so it is opaque.
fn internal() -> AccessError {
    AccessError::new(AccessErrorKind::InternalError, json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: &str = "01234567-89ab-cdef-0123-456789abcdef";
    const SECOND: &str = "11234567-89ab-cdef-0123-456789abcdef";

    fn row(id: &str, created_at: &str) -> (TimestampTz, Uuid) {
        (TimestampTz(created_at.to_owned()), Uuid(id.to_owned()))
    }

    #[test]
    fn the_extra_row_mints_the_cursor_that_starts_the_next_page() {
        let rows = vec![
            row(FIRST, "2026-09-23T12:00:00.000000Z"),
            row(SECOND, "2026-09-23T12:01:00.123456Z"),
        ];
        let page = finish(rows, 1, |(created_at, id)| (created_at, id)).unwrap();
        assert_eq!(page.item.len(), 1);
        let ((created_at, id), limit) = start(page.next_cursor.as_deref(), None).unwrap();
        assert_eq!(created_at.unwrap().0, "2026-09-23T12:00:00.000000Z");
        assert_eq!(id.unwrap().0, FIRST);
        assert_eq!(limit, 100);

        let rows = vec![row(FIRST, "2026-09-23T12:00:00.000000Z")];
        let page = finish(rows, 1, |(created_at, id)| (created_at, id)).unwrap();
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn a_limit_outside_one_to_one_hundred_refuses_with_its_bounds() {
        for limit in [0, 101] {
            let error = start(None, Some(limit)).unwrap_err();
            assert_eq!(error.kind(), AccessErrorKind::InvalidInput);
            assert_eq!(error.detail()["observed"], limit);
        }
        assert!(start(Some("not-a-cursor"), None).is_err());
    }
}

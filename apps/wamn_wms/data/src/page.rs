//! One keyset read in the generated `created_at, id` order.
//!
//! Every model query without authored SQL reads this way: one statement, the
//! tie-breaker on `id`, and a cursor minted from the last row handed out. The
//! statement reads one row past the limit, and that row says whether a next
//! read has rows.

use chrono::{DateTime, Utc};
use serde_json::json;
use wamn_postgres_statements::{Json, RowStream, TimestampTz, Uuid};

use crate::cursor::{self, CursorDirection};
use crate::error::{self, AccessError, AccessErrorKind};

const FIELD: &str = "created_at";

/// Mints the cursor that starts the next read after one row.
type CursorFn<Row> = Box<dyn Fn(&Row) -> Result<String, AccessError>>;

/// The rows of one read, handed out one at a time as the host reads them,
/// and the cursor that continues the read, if anything does.
pub struct Page<Row> {
    rows: RowStream<Row>,
    limit: usize,
    handed: usize,
    cursor: CursorFn<Row>,
    next_cursor: Option<String>,
}

impl<Row> std::fmt::Debug for Page<Row> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Page")
            .field("limit", &self.limit)
            .field("handed", &self.handed)
            .finish_non_exhaustive()
    }
}

impl<Row> Page<Row> {
    /// A read of `limit` rows over a statement that reads `limit + 1`.
    pub(crate) fn new(
        rows: RowStream<Row>,
        limit: i64,
        cursor: impl Fn(&Row) -> Result<String, AccessError> + 'static,
    ) -> Self {
        Self {
            rows,
            limit: usize::try_from(limit).unwrap_or(0),
            handed: 0,
            cursor: Box::new(cursor),
            next_cursor: None,
        }
    }

    /// The next row, or `None` after `limit` rows or the last row.
    ///
    /// # Errors
    ///
    /// [`AccessError`] when the statement fails or a row cannot mint a cursor.
    pub async fn next(&mut self) -> Result<Option<Row>, AccessError> {
        if self.handed == self.limit {
            return Ok(None);
        }
        let Some(row) = self
            .rows
            .next()
            .await
            .map_err(|e| error::from_statement(&e))?
        else {
            self.handed = self.limit;
            return Ok(None);
        };
        self.handed += 1;
        // The row past the limit says whether a next read has rows.
        if self.handed == self.limit
            && self
                .rows
                .next()
                .await
                .map_err(|e| error::from_statement(&e))?
                .is_some()
        {
            self.next_cursor = Some((self.cursor)(&row)?);
        }
        Ok(Some(row))
    }

    /// The cursor that continues this read, once `next` returned `None`.
    pub fn next_cursor(&self) -> Option<String> {
        self.next_cursor.clone()
    }
}

/// The cursor's bindings: the `created_at` and the `id` of the last row.
pub(crate) type Position = (Option<TimestampTz>, Option<Uuid>);

/// Decode the cursor. The operation's codec already checked the limit.
pub(crate) fn start(encoded: Option<&str>) -> Result<Position, AccessError> {
    Ok(match encoded {
        None => (None, None),
        Some(encoded) => {
            let cursor =
                cursor::decode_cursor::<DateTime<Utc>>(encoded, FIELD, CursorDirection::Ascending)?;
            (
                Some(TimestampTz(cursor::canonical_timestamp(&cursor.key))),
                Some(Uuid(cursor.id.hyphenated().to_string())),
            )
        }
    })
}

/// A filter's values, bound as one JSON array.
pub(crate) fn text_json(values: &[String]) -> Json {
    Json(serde_json::to_string(values).expect("strings serialize"))
}

/// The cursor that starts the next read after the row with this key.
pub(crate) fn created_at_cursor(
    created_at: &TimestampTz,
    id: &Uuid,
) -> Result<String, AccessError> {
    let created_at = DateTime::parse_from_rfc3339(&created_at.0)
        .map_err(|_| internal())?
        .to_utc();
    let id = uuid::Uuid::parse_str(&id.0).map_err(|_| internal())?;
    Ok(cursor::encode_cursor(
        FIELD,
        CursorDirection::Ascending,
        &created_at,
        id,
    ))
}

/// A row that cannot mint a cursor is a deployment fault, so it is opaque.
fn internal() -> AccessError {
    AccessError::new(AccessErrorKind::InternalError, json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: &str = "01234567-89ab-cdef-0123-456789abcdef";

    #[test]
    fn the_last_row_mints_the_cursor_that_starts_the_next_read() {
        let cursor = created_at_cursor(
            &TimestampTz("2026-09-23T12:00:00.000000Z".to_owned()),
            &Uuid(FIRST.to_owned()),
        )
        .unwrap();
        let (created_at, id) = start(Some(&cursor)).unwrap();
        assert_eq!(created_at.unwrap().0, "2026-09-23T12:00:00.000000Z");
        assert_eq!(id.unwrap().0, FIRST);
    }

    #[test]
    fn a_cursor_that_does_not_decode_refuses() {
        assert!(start(Some("not-a-cursor")).is_err());
    }
}

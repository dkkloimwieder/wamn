//! Keyset paging for the two fixture model queries.
//!
//! Both queries declare one sort field, `created_at`, in both directions, with
//! the tie-breaker on `id`. The cursor is the closed v1 shape that
//! `generated/contracts/cursor-v1.json` fixes: canonical compact JSON of
//! `{direction, field, id, key, v}` in unpadded base64url. A decode refuses
//! anything it would not have minted itself.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, SecondsFormat};
use serde_json::{Value, json};
use wamn_execution_contract::canonical_json_bytes;
use wamn_postgres_statements::{Json, RowStream, TimestampTz, Uuid};

use crate::error::{AccessError, AccessErrorKind, Constraints};
use crate::scalar;

const FIELD: &str = "created_at";

/// The query body. Every member but the limit is optional; the operation's
/// codec fills the limit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryInput {
    /// Values the one declared filter field must equal, any of them.
    pub filter: Option<Vec<String>>,
    pub sort_field: Option<String>,
    pub sort_direction: Option<String>,
    pub cursor: Option<String>,
    pub limit: i64,
}

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
        let Some(row) = self.read().await? else {
            self.handed = self.limit;
            return Ok(None);
        };
        self.handed += 1;
        // The row past the limit says whether a next read has rows.
        if self.handed == self.limit && self.read().await?.is_some() {
            self.next_cursor = Some((self.cursor)(&row)?);
        }
        Ok(Some(row))
    }

    /// The cursor that continues this read, once `next` returned `None`.
    pub fn next_cursor(&self) -> Option<String> {
        self.next_cursor.clone()
    }

    async fn read(&mut self) -> Result<Option<Row>, AccessError> {
        self.rows
            .next()
            .await
            .map_err(|error| AccessError::from_statement(&error, Constraints::NONE))
    }
}

/// The validated bindings of one page request.
#[derive(Debug)]
pub(crate) struct Plan {
    pub(crate) descending: bool,
    pub(crate) filter: Option<Json>,
    pub(crate) cursor_key: Option<TimestampTz>,
    pub(crate) cursor_id: Option<Uuid>,
    pub(crate) limit: i64,
}

/// Validate one request in the contract's order: sort, then cursor. The
/// operation's codec checked the limit.
pub(crate) fn plan(input: &QueryInput) -> Result<Plan, AccessError> {
    let descending = match (input.sort_field.as_deref(), input.sort_direction.as_deref()) {
        (None, None) | (Some(FIELD), Some("ascending")) => false,
        (Some(FIELD), Some("descending")) => true,
        _ => return Err(invalid("sort")),
    };
    let (cursor_key, cursor_id) = match input.cursor.as_deref() {
        Some(encoded) => {
            let (key, id) = decode(encoded, descending)?;
            (Some(key), Some(id))
        }
        None => (None, None),
    };
    Ok(Plan {
        descending,
        filter: input.filter.as_ref().map(|values| {
            Json(serde_json::to_string(values).expect("a list of strings serializes"))
        }),
        cursor_key,
        cursor_id,
        limit: input.limit,
    })
}

/// The cursor that starts the next read after the row with this key.
pub(crate) fn cursor(
    descending: bool,
    created_at: &TimestampTz,
    id: &Uuid,
) -> Result<String, AccessError> {
    Ok(encode(descending, &timestamp(&created_at.0)?, &id.0))
}

fn encode(descending: bool, key: &str, id: &str) -> String {
    let direction = if descending {
        "descending"
    } else {
        "ascending"
    };
    URL_SAFE_NO_PAD.encode(canonical_json_bytes(&json!({
        "direction": direction,
        "field": FIELD,
        "id": id,
        "key": key,
        "v": 1,
    })))
}

fn decode(encoded: &str, descending: bool) -> Result<(TimestampTz, Uuid), AccessError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| invalid("cursor"))?;
    let wire: Value = serde_json::from_slice(&bytes).map_err(|_| invalid("cursor"))?;
    let member = |name: &str| {
        wire.get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("cursor"))
    };
    let key = timestamp(member("key")?).map_err(|_| invalid("cursor"))?;
    let id = scalar::uuid("cursor", member("id")?)?;
    // Re-minting the cursor refuses another version, field or direction, any
    // other member, and every spelling that is not the one this crate writes.
    if encode(descending, &key, &id.0) != encoded {
        return Err(invalid("cursor"));
    }
    Ok((TimestampTz(key), id))
}

/// The canonical spelling of one instant: UTC with six fractional digits.
fn timestamp(value: &str) -> Result<String, AccessError> {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.to_utc().to_rfc3339_opts(SecondsFormat::Micros, true))
        .map_err(|_| AccessError::internal())
}

fn invalid(field: &str) -> AccessError {
    AccessError::field(AccessErrorKind::InvalidInput, field)
}

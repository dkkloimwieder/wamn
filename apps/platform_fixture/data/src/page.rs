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
use wamn_postgres_statements::{Json, TimestampTz, Uuid};

use crate::error::{AccessError, AccessErrorKind};
use crate::scalar;

const FIELD: &str = "created_at";
const MAX_PAGE_SIZE: i64 = 100;

/// The query body. Every member is optional.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryInput {
    /// Values the one declared filter field must equal, any of them.
    pub filter: Option<Vec<String>>,
    pub sort_field: Option<String>,
    pub sort_direction: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

/// One bounded page and the cursor that continues it, if anything does.
#[derive(Debug)]
pub struct Page<Row> {
    pub item: Vec<Row>,
    pub next_cursor: Option<String>,
}

/// The validated bindings of one page request.
#[derive(Debug)]
pub(crate) struct Plan {
    pub(crate) descending: bool,
    pub(crate) filter: Option<Json>,
    pub(crate) cursor_key: Option<TimestampTz>,
    pub(crate) cursor_id: Option<Uuid>,
    /// One row past the page tells whether a next page exists.
    pub(crate) fetch: i64,
}

/// Validate one request in the contract's order: sort, cursor, then limit.
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
    let limit = input.limit.unwrap_or(MAX_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(AccessError::range("limit", 1, MAX_PAGE_SIZE, limit));
    }
    Ok(Plan {
        descending,
        filter: input.filter.as_ref().map(|values| {
            Json(serde_json::to_string(values).expect("a list of strings serializes"))
        }),
        cursor_key,
        cursor_id,
        fetch: limit + 1,
    })
}

/// Keep one page of rows and mint the cursor from the last row kept.
pub(crate) fn finish<Row>(
    plan: &Plan,
    mut rows: Vec<Row>,
    key: impl Fn(&Row) -> (&TimestampTz, &Uuid),
) -> Result<Page<Row>, AccessError> {
    let limit = usize::try_from(plan.fetch - 1).expect("a validated page limit fits usize");
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = match rows.last().filter(|_| more) {
        Some(row) => {
            let (created_at, id) = key(row);
            Some(encode(plan.descending, &timestamp(&created_at.0)?, &id.0))
        }
        None => None,
    };
    Ok(Page {
        item: rows,
        next_cursor,
    })
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

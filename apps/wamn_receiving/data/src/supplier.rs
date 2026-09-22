//! Runtime-checked `supplier` operations.

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::json;
use uuid::Uuid;
use wamn_execution_contract::canonical_json_bytes;
use wamn_postgres_statements::{Connection, TimestampTz, Uuid as WamnUuid};

use crate::cursor::{CursorDirection, decode_cursor, encode_cursor};
use crate::error::{AccessError, AllowedConstraints};
use crate::generated::wamn::supplier as generated;

#[doc(inline)]
pub use crate::generated::wamn::supplier::SupplierRow;

const MAX_PAGE_SIZE: i64 = 100;
const CREATE_CONSTRAINTS: AllowedConstraints = AllowedConstraints::new(
    generated::CREATE_UNIQUE_CONSTRAINTS,
    generated::CREATE_FOREIGN_KEY_CONSTRAINTS,
    generated::CREATE_CHECK_CONSTRAINTS,
    generated::CREATE_EXCLUSION_CONSTRAINTS,
);

/// Typed supplier query input.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryInput {
    pub cursor: Option<Box<str>>,
    pub limit: Option<i64>,
}

/// One bounded supplier page and its opaque continuation cursor.
#[derive(Debug)]
pub struct Page {
    pub item: Box<[SupplierRow]>,
    pub next_cursor: Option<Box<str>>,
}

/// Query one bounded page in generated `created_at, id` order.
pub async fn query(connection: &mut Connection, input: &QueryInput) -> Result<Page, AccessError> {
    let cursor = input
        .cursor
        .as_deref()
        .map(|cursor| {
            decode_cursor::<DateTime<Utc>>(cursor, "created_at", CursorDirection::Ascending)
        })
        .transpose()?;
    let limit = validated_limit(input.limit)?;
    let page_size = usize::try_from(limit).expect("validated supplier page limit fits usize");
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
                AccessError::from_statement("query supplier", &source, AllowedConstraints::NONE)
            })?;
    page_from_rows(rows, page_size)
}

/// Create one supplier under the identity its claim row mints.
///
/// The key carries the command, so a retry of the same name returns the row the
/// first attempt wrote, and the same key under a different name refuses.
pub async fn create(
    connection: &mut Connection,
    idempotency_key: &str,
    name: Option<&str>,
) -> Result<SupplierRow, AccessError> {
    let name = validated_name(name)?;
    let canonical_command = canonical_json_bytes(&json!({"name": name}));
    let transaction = connection.begin().await.map_err(|source| {
        AccessError::from_statement("begin supplier creation", &source, CREATE_CONSTRAINTS)
    })?;
    let mut claim = generated::begin_claim(transaction);

    if let Some(replay) = generated::create_replay(&mut claim, idempotency_key.to_owned())
        .await
        .map_err(|source| {
            AccessError::from_statement("find supplier replay", &source, CREATE_CONSTRAINTS)
        })?
    {
        return replay_row(replay, &canonical_command);
    }

    let claimed = generated::create_claim(
        &mut claim,
        idempotency_key.to_owned(),
        canonical_command.clone(),
    )
    .await
    .map_err(|source| {
        AccessError::from_statement(
            "claim supplier idempotency key",
            &source,
            CREATE_CONSTRAINTS,
        )
    })?;
    let Some(claimed) = claimed else {
        // Another caller took the key between the replay read and this insert.
        // Its row is committed, so the replay read now answers.
        let replay = generated::create_replay(&mut claim, idempotency_key.to_owned())
            .await
            .map_err(|source| {
                AccessError::from_statement(
                    "load concurrent supplier replay",
                    &source,
                    CREATE_CONSTRAINTS,
                )
            })?
            .ok_or_else(|| {
                AccessError::internal("conflicting supplier claim has no durable row")
            })?;
        return replay_row(replay, &canonical_command);
    };

    let finalized = generated::create(claim, claimed.supplier_id, name.to_owned())
        .await
        .map_err(|source| {
            AccessError::from_statement("insert supplier", &source, CREATE_CONSTRAINTS)
        })?;
    let row = SupplierRow {
        created_at: finalized.row.created_at.clone(),
        id: finalized.row.id.clone(),
        name: finalized.row.name.clone(),
    };
    finalized.commit().await.map_err(|source| {
        AccessError::from_statement("commit supplier creation", &source, CREATE_CONSTRAINTS)
    })?;
    Ok(row)
}

/// The column is `NOT NULL` and carries no default, so a name the caller left
/// out or sent as null is a refusal the caller can correct, not a server fault.
fn validated_name(name: Option<&str>) -> Result<&str, AccessError> {
    let name = name.ok_or_else(|| AccessError::invalid("supplier name is required", "name"))?;
    if name.trim().is_empty() {
        return Err(AccessError::invalid("supplier name is empty", "name"));
    }
    Ok(name)
}

fn replay_row(
    replay: generated::SupplierCreateReplayRow,
    canonical_command: &[u8],
) -> Result<SupplierRow, AccessError> {
    if replay.canonical_command != canonical_command {
        return Err(AccessError::idempotency_conflict(
            "idempotency_key is already bound to a different supplier",
            "idempotency_key",
        ));
    }
    Ok(SupplierRow {
        created_at: replay.created_at,
        id: replay.id,
        name: replay.name,
    })
}

fn validated_limit(limit: Option<i64>) -> Result<i64, AccessError> {
    let limit = limit.unwrap_or(MAX_PAGE_SIZE);
    if (1..=MAX_PAGE_SIZE).contains(&limit) {
        Ok(limit)
    } else {
        Err(AccessError::invalid_range(
            "supplier limit must be 1..=100",
            "limit",
            1,
            MAX_PAGE_SIZE,
            limit,
        ))
    }
}

fn page_from_rows(mut rows: Vec<SupplierRow>, page_size: usize) -> Result<Page, AccessError> {
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

fn cursor_from_row(row: &SupplierRow) -> Result<Box<str>, AccessError> {
    let created_at = DateTime::parse_from_rfc3339(&row.created_at.0)
        .map(|timestamp| timestamp.to_utc())
        .map_err(|_| AccessError::internal("supplier row contains an invalid created_at"))?;
    let id = Uuid::parse_str(&row.id.0)
        .ok()
        .filter(|parsed| parsed.hyphenated().to_string() == row.id.0)
        .ok_or_else(|| AccessError::internal("supplier row contains a noncanonical id"))?;
    encode_cursor("created_at", CursorDirection::Ascending, &created_at, id)
        .map(String::into_boxed_str)
}

#[cfg(test)]
mod tests {
    use super::{AccessError, SupplierRow, page_from_rows, validated_limit, validated_name};
    use crate::cursor::{CursorDirection, DecodedCursor, decode_cursor};
    use crate::error::AccessErrorKind;
    use chrono::{DateTime, Utc};
    use uuid::Uuid;
    use wamn_postgres_statements::{TimestampTz, Uuid as WamnUuid};

    const FIRST_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
    const SECOND_ID: &str = "11234567-89ab-cdef-0123-456789abcdef";
    const THIRD_ID: &str = "21234567-89ab-cdef-0123-456789abcdef";

    #[test]
    fn a_missing_or_blank_name_refuses_on_its_own_field() {
        assert_eq!(validated_name(Some("Acme")).unwrap(), "Acme");
        for name in [None, Some(""), Some("   ")] {
            let error: AccessError = validated_name(name).unwrap_err();
            assert_eq!(error.kind(), AccessErrorKind::InvalidInput);
            assert_eq!(error.field(), Some("name"));
        }
    }

    #[test]
    fn limit_defaults_and_refuses_outside_the_closed_range() {
        assert_eq!(validated_limit(None).unwrap(), 100);
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
                row(FIRST_ID, "2026-09-22T12:00:00.000000Z"),
                row(SECOND_ID, "2026-09-22T12:01:00.123456Z"),
                row(THIRD_ID, "2026-09-22T12:02:00.000000Z"),
            ],
            2,
        )
        .unwrap();

        assert_eq!(page.item.len(), 2);
        let decoded = decode_cursor::<DateTime<Utc>>(
            page.next_cursor.as_deref().unwrap(),
            "created_at",
            CursorDirection::Ascending,
        )
        .unwrap();
        assert_eq!(
            decoded,
            DecodedCursor {
                key: DateTime::parse_from_rfc3339("2026-09-22T12:01:00.123456Z")
                    .unwrap()
                    .to_utc(),
                id: Uuid::parse_str(SECOND_ID).unwrap(),
            }
        );
    }

    fn row(id: &str, created_at: &str) -> SupplierRow {
        SupplierRow {
            created_at: TimestampTz(created_at.to_owned()),
            id: WamnUuid(id.to_owned()),
            name: "Acme Supply".to_owned(),
        }
    }
}

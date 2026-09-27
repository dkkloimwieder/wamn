//! Runtime-checked `supplier` operations.

use chrono::{DateTime, SecondsFormat, Utc};
use uuid::Uuid;
use wamn_postgres_statements::{Connection, TimestampTz, Transaction, Uuid as WamnUuid};

use crate::cursor::{CursorDirection, decode_cursor, encode_cursor};
use crate::error::{AccessError, AllowedConstraints};
use crate::generated::wamn::supplier as generated;
use crate::page::Page;

#[doc(inline)]
pub use crate::generated::wamn::supplier::SupplierRow;

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
    pub limit: i64,
}

/// Query one read in generated `created_at, id` order.
pub async fn query(
    connection: &mut Connection,
    input: &QueryInput,
) -> Result<Page<SupplierRow>, AccessError> {
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
                AccessError::from_statement("query supplier", &source, AllowedConstraints::NONE)
            })?;
    Ok(Page::new(rows, "query supplier", limit, cursor_from_row))
}

/// Create one supplier in the transaction its codec holds for the write log.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub async fn create(
    transaction: &mut Transaction,
    name: Option<&str>,
) -> Result<SupplierRow, AccessError> {
    let name = validated_name(name)?;
    generated::create(transaction, name.to_owned())
        .await
        .map_err(|source| {
            AccessError::from_statement("insert supplier", &source, CREATE_CONSTRAINTS)
        })
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
    use super::{AccessError, SupplierRow, cursor_from_row, validated_name};
    use crate::cursor::{CursorDirection, DecodedCursor, decode_cursor};
    use crate::error::AccessErrorKind;
    use chrono::{DateTime, Utc};
    use uuid::Uuid;
    use wamn_postgres_statements::{TimestampTz, Uuid as WamnUuid};

    const SECOND_ID: &str = "11234567-89ab-cdef-0123-456789abcdef";

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
    fn the_last_row_mints_the_cursor_that_starts_the_next_read() {
        let cursor = cursor_from_row(&row(SECOND_ID, "2026-09-22T12:01:00.123456Z")).unwrap();
        let decoded =
            decode_cursor::<DateTime<Utc>>(&cursor, "created_at", CursorDirection::Ascending)
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

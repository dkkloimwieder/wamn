//! Runtime-checked `purchase_order` operations.

use chrono::{DateTime, SecondsFormat, Utc};
use wamn_postgres_statements::{Connection, Json, TimestampTz, Uuid as WamnUuid};

use crate::cursor::{CursorDirection, CursorKey, DecodedCursor, decode_cursor, encode_cursor};
use crate::error::{AccessError, AllowedConstraints};
use crate::generated::wamn::purchase_order as generated;
use crate::page::Page;

pub use crate::generated::wamn::purchase_order::PurchaseOrderRow;

pub(crate) const UPDATE_CONSTRAINTS: AllowedConstraints = AllowedConstraints::new(
    generated::UPDATE_UNIQUE_CONSTRAINTS,
    generated::UPDATE_FOREIGN_KEY_CONSTRAINTS,
    generated::UPDATE_CHECK_CONSTRAINTS,
    generated::UPDATE_EXCLUSION_CONSTRAINTS,
);

/// Closed `purchase_order.status` vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PurchaseOrderStatus {
    Open,
    Complete,
    Cancelled,
}

impl PurchaseOrderStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Complete => "complete",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Finite SQL ordering declared by the Receiving manifest.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PurchaseOrderSort {
    PurchaseOrderNumberAscending,
    PurchaseOrderNumberDescending,
    StatusAscending,
    StatusDescending,
    #[default]
    CreatedAtAscending,
    CreatedAtDescending,
}

impl PurchaseOrderSort {
    const fn field(self) -> &'static str {
        match self {
            Self::PurchaseOrderNumberAscending | Self::PurchaseOrderNumberDescending => {
                "purchase_order_number"
            }
            Self::StatusAscending | Self::StatusDescending => "status",
            Self::CreatedAtAscending | Self::CreatedAtDescending => "created_at",
        }
    }

    const fn direction(self) -> CursorDirection {
        match self {
            Self::PurchaseOrderNumberAscending
            | Self::StatusAscending
            | Self::CreatedAtAscending => CursorDirection::Ascending,
            Self::PurchaseOrderNumberDescending
            | Self::StatusDescending
            | Self::CreatedAtDescending => CursorDirection::Descending,
        }
    }
}

/// Typed query input; cursors remain opaque outside their minting operation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryInput {
    pub supplier_ids: Option<Box<[Box<str>]>>,
    pub statuses: Option<Box<[PurchaseOrderStatus]>>,
    pub purchase_order_numbers: Option<Box<[Box<str>]>>,
    pub sort: PurchaseOrderSort,
    pub cursor: Option<Box<str>>,
    pub limit: i64,
}

/// Three-state update input for the sole writable field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SupplierIdUpdate {
    Omitted,
    Null,
    Value(Box<str>),
}

/// Load one purchase order by UUID, in any accepted spelling.
pub async fn get(connection: &mut Connection, id: &str) -> Result<PurchaseOrderRow, AccessError> {
    let id = canonical_uuid(id, "purchase_order id", "id")?;
    generated::get(connection, WamnUuid(id))
        .await
        .map_err(|source| {
            AccessError::from_statement("load purchase_order", &source, AllowedConstraints::NONE)
        })?
        .ok_or_else(|| AccessError::not_found("purchase_order does not exist"))
}

/// Query one read using a finite generated SQL variant.
pub async fn query(
    connection: &mut Connection,
    input: &QueryInput,
) -> Result<Page<PurchaseOrderRow>, AccessError> {
    let prepared = prepare_query(input)?;
    let rows = match (input.sort, prepared.cursor) {
        (PurchaseOrderSort::PurchaseOrderNumberAscending, QueryCursor::Text(cursor)) => {
            let (cursor_key, cursor_id) = text_cursor_bindings(cursor);
            generated::query_purchase_order_number_ascending(
                connection,
                prepared.supplier_ids,
                prepared.statuses,
                prepared.purchase_order_numbers,
                cursor_key,
                cursor_id,
                prepared.fetch_limit,
            )
            .await
        }
        (PurchaseOrderSort::PurchaseOrderNumberDescending, QueryCursor::Text(cursor)) => {
            let (cursor_key, cursor_id) = text_cursor_bindings(cursor);
            generated::query_purchase_order_number_descending(
                connection,
                prepared.supplier_ids,
                prepared.statuses,
                prepared.purchase_order_numbers,
                cursor_key,
                cursor_id,
                prepared.fetch_limit,
            )
            .await
        }
        (PurchaseOrderSort::StatusAscending, QueryCursor::Text(cursor)) => {
            let (cursor_key, cursor_id) = text_cursor_bindings(cursor);
            generated::query_status_ascending(
                connection,
                prepared.supplier_ids,
                prepared.statuses,
                prepared.purchase_order_numbers,
                cursor_key,
                cursor_id,
                prepared.fetch_limit,
            )
            .await
        }
        (PurchaseOrderSort::StatusDescending, QueryCursor::Text(cursor)) => {
            let (cursor_key, cursor_id) = text_cursor_bindings(cursor);
            generated::query_status_descending(
                connection,
                prepared.supplier_ids,
                prepared.statuses,
                prepared.purchase_order_numbers,
                cursor_key,
                cursor_id,
                prepared.fetch_limit,
            )
            .await
        }
        (PurchaseOrderSort::CreatedAtAscending, QueryCursor::Timestamp(cursor)) => {
            let (cursor_key, cursor_id) = timestamp_cursor_bindings(cursor);
            generated::query_created_at_ascending(
                connection,
                prepared.supplier_ids,
                prepared.statuses,
                prepared.purchase_order_numbers,
                cursor_key,
                cursor_id,
                prepared.fetch_limit,
            )
            .await
        }
        (PurchaseOrderSort::CreatedAtDescending, QueryCursor::Timestamp(cursor)) => {
            let (cursor_key, cursor_id) = timestamp_cursor_bindings(cursor);
            generated::query_created_at_descending(
                connection,
                prepared.supplier_ids,
                prepared.statuses,
                prepared.purchase_order_numbers,
                cursor_key,
                cursor_id,
                prepared.fetch_limit,
            )
            .await
        }
        _ => unreachable!("sort selects exactly one cursor key type"),
    }
    .map_err(|source| {
        AccessError::from_statement("query purchase_order", &source, AllowedConstraints::NONE)
    })?;
    let sort = input.sort;
    Ok(Page::new(
        rows,
        "query purchase_order",
        input.limit,
        move |row| encode_row_cursor(row, sort),
    ))
}

/// Apply one optimistic update without retrying serialization failures.
pub async fn update(
    connection: &mut Connection,
    id: &str,
    expected_row_version: i32,
    supplier_id: SupplierIdUpdate,
) -> Result<PurchaseOrderRow, AccessError> {
    let id = canonical_uuid(id, "purchase_order id", "id")?;
    let (supplier_id_present, supplier_id) = supplier_update(supplier_id)?;
    let row = generated::update(
        connection,
        WamnUuid(id),
        expected_row_version,
        supplier_id_present,
        supplier_id,
    )
    .await
    .map_err(|source| {
        AccessError::from_statement("update purchase_order", &source, UPDATE_CONSTRAINTS)
    })?;
    update_result(row, expected_row_version)
}

#[derive(Debug)]
struct PreparedQuery {
    cursor: QueryCursor,
    supplier_ids: Option<Json>,
    statuses: Option<Json>,
    purchase_order_numbers: Option<Json>,
    fetch_limit: i64,
}

#[derive(Debug)]
enum QueryCursor {
    Text(Option<DecodedCursor<Box<str>>>),
    Timestamp(Option<DecodedCursor<DateTime<Utc>>>),
}

fn prepare_query(input: &QueryInput) -> Result<PreparedQuery, AccessError> {
    let cursor = decode_query_cursor(input.sort, input.cursor.as_deref())?;
    // The operation's codec checked the limit.
    let limit = input.limit;
    let supplier_ids = supplier_filter(input.supplier_ids.as_deref())?;
    let statuses = status_filter(input.statuses.as_deref());
    let purchase_order_numbers = number_filter(input.purchase_order_numbers.as_deref())?;
    Ok(PreparedQuery {
        cursor,
        supplier_ids,
        statuses,
        purchase_order_numbers,
        fetch_limit: limit + 1,
    })
}

fn decode_query_cursor(
    sort: PurchaseOrderSort,
    encoded: Option<&str>,
) -> Result<QueryCursor, AccessError> {
    match sort {
        PurchaseOrderSort::PurchaseOrderNumberAscending
        | PurchaseOrderSort::PurchaseOrderNumberDescending => {
            decode_optional_cursor::<Box<str>>(encoded, sort.field(), sort.direction())
                .map(QueryCursor::Text)
        }
        PurchaseOrderSort::StatusAscending | PurchaseOrderSort::StatusDescending => {
            let cursor =
                decode_optional_cursor::<Box<str>>(encoded, sort.field(), sort.direction())?;
            if let Some(cursor) = &cursor {
                validate_status(&cursor.key)?;
            }
            Ok(QueryCursor::Text(cursor))
        }
        PurchaseOrderSort::CreatedAtAscending | PurchaseOrderSort::CreatedAtDescending => {
            decode_optional_cursor::<DateTime<Utc>>(encoded, sort.field(), sort.direction())
                .map(QueryCursor::Timestamp)
        }
    }
}

fn decode_optional_cursor<Key: CursorKey>(
    encoded: Option<&str>,
    field: &str,
    direction: CursorDirection,
) -> Result<Option<DecodedCursor<Key>>, AccessError> {
    encoded
        .map(|encoded| decode_cursor(encoded, field, direction))
        .transpose()
}

fn text_cursor_bindings(
    cursor: Option<DecodedCursor<Box<str>>>,
) -> (Option<String>, Option<WamnUuid>) {
    match cursor {
        Some(cursor) => (
            Some(cursor.key.into()),
            Some(WamnUuid(cursor.id.hyphenated().to_string())),
        ),
        None => (None, None),
    }
}

fn timestamp_cursor_bindings(
    cursor: Option<DecodedCursor<DateTime<Utc>>>,
) -> (Option<TimestampTz>, Option<WamnUuid>) {
    match cursor {
        Some(cursor) => (
            Some(TimestampTz(canonical_timestamp(&cursor.key))),
            Some(WamnUuid(cursor.id.hyphenated().to_string())),
        ),
        None => (None, None),
    }
}

fn supplier_filter(values: Option<&[Box<str>]>) -> Result<Option<Json>, AccessError> {
    values
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    canonical_uuid(
                        value,
                        "purchase_order supplier filter",
                        "filter.supplier_id",
                    )
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|values| Json(serde_json::to_string(&values).expect("strings serialize")))
        })
        .transpose()
}

fn status_filter(values: Option<&[PurchaseOrderStatus]>) -> Option<Json> {
    values.map(|values| {
        let values = values
            .iter()
            .map(|status| status.as_str())
            .collect::<Vec<_>>();
        Json(serde_json::to_string(&values).expect("status literals serialize"))
    })
}

/// The declared `purchase_order_number` filter, which is the free text an
/// operator types and which matches by contains. An empty value is a part of
/// every order number, so it refuses instead of returning every order as if
/// it matched.
fn number_filter(values: Option<&[Box<str>]>) -> Result<Option<Json>, AccessError> {
    values
        .map(|values| {
            if values.iter().any(|value| value.is_empty()) {
                return Err(AccessError::invalid(
                    "purchase_order_number filter carries an empty value",
                    "filter.purchase_order_number",
                ));
            }
            Ok(Json(
                serde_json::to_string(&values).expect("strings serialize"),
            ))
        })
        .transpose()
}

fn supplier_update(supplier_id: SupplierIdUpdate) -> Result<(bool, Option<WamnUuid>), AccessError> {
    match supplier_id {
        SupplierIdUpdate::Omitted => Ok((false, None)),
        SupplierIdUpdate::Null => Err(AccessError::invalid(
            "purchase_order supplier_id does not accept explicit null",
            "change.supplier_id",
        )),
        SupplierIdUpdate::Value(value) => Ok((
            true,
            Some(WamnUuid(canonical_uuid(
                &value,
                "purchase_order supplier_id",
                "change.supplier_id",
            )?)),
        )),
    }
}

fn encode_row_cursor(
    row: &PurchaseOrderRow,
    sort: PurchaseOrderSort,
) -> Result<Box<str>, AccessError> {
    let id = row_uuid(&row.id)?;
    let encoded = match sort {
        PurchaseOrderSort::PurchaseOrderNumberAscending
        | PurchaseOrderSort::PurchaseOrderNumberDescending => {
            let key = row.purchase_order_number.clone().into_boxed_str();
            encode_cursor(sort.field(), sort.direction(), &key, id)
        }
        PurchaseOrderSort::StatusAscending | PurchaseOrderSort::StatusDescending => {
            validate_row_status(&row.status)?;
            let key = row.status.clone().into_boxed_str();
            encode_cursor(sort.field(), sort.direction(), &key, id)
        }
        PurchaseOrderSort::CreatedAtAscending | PurchaseOrderSort::CreatedAtDescending => {
            let key = row_timestamp(&row.created_at)?;
            encode_cursor(sort.field(), sort.direction(), &key, id)
        }
    }
    .map_err(|_| AccessError::internal("purchase_order row could not mint a cursor"))?;
    Ok(encoded.into_boxed_str())
}

fn validate_status(value: &str) -> Result<(), AccessError> {
    if matches!(value, "open" | "complete" | "cancelled") {
        Ok(())
    } else {
        Err(AccessError::invalid(
            "purchase_order status cursor is outside the closed vocabulary",
            "cursor",
        ))
    }
}

fn validate_row_status(value: &str) -> Result<(), AccessError> {
    if matches!(value, "open" | "complete" | "cancelled") {
        Ok(())
    } else {
        Err(AccessError::internal(
            "purchase_order row status is outside the closed vocabulary",
        ))
    }
}

/// Parse any accepted UUID spelling and re-spell it lowercase-hyphenated.
///
/// Case is representation, so an input arrives in any spelling and reaches the
/// database in one.
fn canonical_uuid(value: &str, context: &str, field: &'static str) -> Result<String, AccessError> {
    uuid::Uuid::parse_str(value)
        .map(|parsed| parsed.hyphenated().to_string())
        .map_err(|_| AccessError::invalid(format!("{context} is not a UUID"), field))
}

fn row_uuid(value: &WamnUuid) -> Result<uuid::Uuid, AccessError> {
    uuid::Uuid::parse_str(&value.0)
        .ok()
        .filter(|parsed| parsed.hyphenated().to_string() == value.0)
        .ok_or_else(|| AccessError::internal("purchase_order row id is not a canonical UUID"))
}

fn row_timestamp(value: &TimestampTz) -> Result<DateTime<Utc>, AccessError> {
    DateTime::parse_from_rfc3339(&value.0)
        .map(|timestamp| timestamp.to_utc())
        .map_err(|_| AccessError::internal("purchase_order row created_at is not RFC3339"))
}

fn canonical_timestamp(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn update_result(
    row: generated::PurchaseOrderUpdateRow,
    expected_row_version: i32,
) -> Result<PurchaseOrderRow, AccessError> {
    match row.outcome.as_deref() {
        Some("not_found") => Err(AccessError::not_found("purchase_order does not exist")),
        Some("concurrency_conflict") => match row.observed_row_version {
            Some(observed_row_version) => Err(AccessError::concurrency_conflict(
                format!(
                    "purchase_order row_version {observed_row_version} does not match {expected_row_version}"
                ),
                observed_row_version,
            )),
            None => Err(AccessError::internal(
                "purchase_order concurrency refusal omitted observed_row_version",
            )),
        },
        Some("updated") => match (
            row.id,
            row.purchase_order_number,
            row.supplier_id,
            row.status,
            row.row_version,
            row.created_at,
            row.created_by,
            row.updated_at,
            row.updated_by,
        ) {
            (
                Some(id),
                Some(purchase_order_number),
                Some(supplier_id),
                Some(status),
                Some(row_version),
                Some(created_at),
                Some(created_by),
                Some(updated_at),
                Some(updated_by),
            ) => Ok(PurchaseOrderRow {
                created_at,
                created_by,
                id,
                purchase_order_number,
                row_version,
                status,
                supplier_id,
                updated_at,
                updated_by,
            }),
            _ => Err(AccessError::internal(
                "purchase_order update returned an incomplete row",
            )),
        },
        _ => Err(AccessError::internal(
            "purchase_order.update returned a null or unknown outcome",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AccessErrorKind;

    const FIRST_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
    const SECOND_ID: &str = "11234567-89ab-cdef-0123-456789abcdef";

    #[test]
    fn cursor_and_filter_refusals_are_typed() {
        let input = QueryInput {
            supplier_ids: Some(vec!["not-a-uuid".into()].into_boxed_slice()),
            statuses: None,
            purchase_order_numbers: None,
            sort: PurchaseOrderSort::CreatedAtAscending,
            cursor: Some("not-base64".into()),
            limit: 1,
        };
        assert_eq!(
            prepare_query(&input).unwrap_err().kind(),
            AccessErrorKind::InvalidInput
        );
        assert_eq!(
            supplier_filter(Some(&["not-a-uuid".into()]))
                .unwrap_err()
                .kind(),
            AccessErrorKind::InvalidInput
        );
        let empty_number = number_filter(Some(&["".into()])).unwrap_err();
        assert_eq!(empty_number.kind(), AccessErrorKind::InvalidInput);
        assert_eq!(empty_number.field(), Some("filter.purchase_order_number"));
        assert!(number_filter(Some(&["PO-0001".into()])).unwrap().is_some());
        assert!(number_filter(None).unwrap().is_none());
    }

    #[test]
    fn cursor_field_direction_and_key_type_are_bound_to_the_sort() {
        let id = uuid::Uuid::parse_str(FIRST_ID).unwrap();
        let status: Box<str> = "open".into();
        let encoded = encode_cursor("status", CursorDirection::Descending, &status, id).unwrap();

        assert!(matches!(
            decode_query_cursor(PurchaseOrderSort::StatusDescending, Some(encoded.as_str()))
                .unwrap(),
            QueryCursor::Text(Some(_))
        ));
        for sort in [
            PurchaseOrderSort::StatusAscending,
            PurchaseOrderSort::CreatedAtDescending,
        ] {
            assert_eq!(
                decode_query_cursor(sort, Some(encoded.as_str()))
                    .unwrap_err()
                    .kind(),
                AccessErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn the_last_row_mints_the_cursor_and_normalizes_utc() {
        let row = row(FIRST_ID, "2026-08-29T12:34:56.123456Z");
        let encoded = encode_row_cursor(&row, PurchaseOrderSort::CreatedAtAscending).unwrap();

        let cursor =
            decode_cursor::<DateTime<Utc>>(&encoded, "created_at", CursorDirection::Ascending)
                .unwrap();
        assert_eq!(cursor.id.hyphenated().to_string(), FIRST_ID);
        assert_eq!(cursor.key.timestamp_subsec_micros(), 123_456);
        assert_eq!(
            canonical_timestamp(&cursor.key),
            "2026-08-29T12:34:56.123456Z"
        );
    }

    #[test]
    fn supplier_update_preserves_omitted_null_and_value_states() {
        assert_eq!(
            supplier_update(SupplierIdUpdate::Omitted).unwrap(),
            (false, None)
        );
        assert_eq!(
            supplier_update(SupplierIdUpdate::Null).unwrap_err().kind(),
            AccessErrorKind::InvalidInput
        );
        assert_eq!(
            supplier_update(SupplierIdUpdate::Value(FIRST_ID.into())).unwrap(),
            (true, Some(WamnUuid(FIRST_ID.to_owned())))
        );
    }

    #[test]
    fn generated_update_outcomes_map_to_the_closed_contract() {
        for (outcome, expected) in [
            ("not_found", AccessErrorKind::NotFound),
            ("concurrency_conflict", AccessErrorKind::ConcurrencyConflict),
            ("unknown", AccessErrorKind::InternalError),
        ] {
            assert_eq!(
                update_result(update_row(outcome, false), 1)
                    .unwrap_err()
                    .kind(),
                expected
            );
        }
        assert_eq!(
            update_result(update_row("updated", false), 1)
                .unwrap_err()
                .kind(),
            AccessErrorKind::InternalError
        );
        let updated = update_result(update_row("updated", true), 1).unwrap();
        assert_eq!(updated.id.0, FIRST_ID);
        assert_eq!(updated.supplier_id.0, SECOND_ID);
        assert_eq!(updated.row_version, 2);
    }

    fn row(id: &str, created_at: &str) -> PurchaseOrderRow {
        PurchaseOrderRow {
            created_at: TimestampTz(created_at.to_owned()),
            created_by: WamnUuid(SECOND_ID.to_owned()),
            id: WamnUuid(id.to_owned()),
            purchase_order_number: "PO-100".to_owned(),
            row_version: 1,
            status: "open".to_owned(),
            supplier_id: WamnUuid(SECOND_ID.to_owned()),
            updated_at: TimestampTz(created_at.to_owned()),
            updated_by: WamnUuid(SECOND_ID.to_owned()),
        }
    }

    fn update_row(outcome: &str, complete: bool) -> generated::PurchaseOrderUpdateRow {
        generated::PurchaseOrderUpdateRow {
            outcome: Some(outcome.to_owned()),
            observed_row_version: (outcome == "concurrency_conflict").then_some(2),
            created_at: complete.then(|| TimestampTz("2026-08-29T12:34:56.000000Z".to_owned())),
            created_by: complete.then(|| WamnUuid(SECOND_ID.to_owned())),
            id: complete.then(|| WamnUuid(FIRST_ID.to_owned())),
            purchase_order_number: complete.then(|| "PO-100".to_owned()),
            row_version: complete.then_some(2),
            status: complete.then(|| "open".to_owned()),
            supplier_id: complete.then(|| WamnUuid(SECOND_ID.to_owned())),
            updated_at: complete.then(|| TimestampTz("2026-08-29T12:35:56.000000Z".to_owned())),
            updated_by: complete.then(|| WamnUuid(SECOND_ID.to_owned())),
        }
    }
}

//! Runtime-checked `purchase_order` operations.

use chrono::{DateTime, SecondsFormat, Utc};
use wamn_postgres_sqlx::{Json, TimestampTz, Uuid as WamnUuid, WamnConnection};

use crate::cursor::{CursorDirection, CursorKey, DecodedCursor, decode_cursor, encode_cursor};
use crate::error::{AccessError, AllowedConstraints};
use crate::generated::wamn::purchase_order as generated;

pub use crate::generated::wamn::purchase_order::PurchaseOrderRow;

const MAX_PAGE_SIZE: i64 = 100;
const UPDATE_CONSTRAINTS: AllowedConstraints = AllowedConstraints::new(
    generated::UPDATE_UNIQUE_CONSTRAINTS,
    generated::UPDATE_FOREIGN_KEY_CONSTRAINTS,
    generated::UPDATE_CHECK_CONSTRAINTS,
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
    pub sort: PurchaseOrderSort,
    pub cursor: Option<Box<str>>,
    pub limit: Option<i64>,
}

/// One bounded query result and its opaque continuation cursor.
#[derive(Debug)]
pub struct Page {
    pub item: Box<[PurchaseOrderRow]>,
    pub next_cursor: Option<Box<str>>,
}

/// Three-state update input for the sole writable field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SupplierIdUpdate {
    Omitted,
    Null,
    Value(Box<str>),
}

/// Load one purchase order by canonical UUID.
pub async fn get(
    connection: &mut WamnConnection,
    id: &str,
) -> Result<PurchaseOrderRow, AccessError> {
    let id = canonical_uuid(id, "purchase_order id")?;
    generated::get(connection, WamnUuid(id))
        .await
        .map_err(|source| {
            AccessError::from_sqlx("load purchase_order", &source, AllowedConstraints::NONE)
        })?
        .ok_or_else(|| AccessError::not_found("purchase_order does not exist"))
}

/// Query one bounded page using a finite generated SQL variant.
pub async fn query(
    connection: &mut WamnConnection,
    input: &QueryInput,
) -> Result<Page, AccessError> {
    let prepared = prepare_query(input)?;
    let mut rows = match (input.sort, prepared.cursor) {
        (PurchaseOrderSort::PurchaseOrderNumberAscending, QueryCursor::Text(cursor)) => {
            let (cursor_key, cursor_id) = text_cursor_bindings(cursor);
            generated::query_purchase_order_number_ascending(
                connection,
                prepared.supplier_ids,
                prepared.statuses,
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
                cursor_key,
                cursor_id,
                prepared.fetch_limit,
            )
            .await
        }
        _ => unreachable!("sort selects exactly one cursor key type"),
    }
    .map_err(|source| {
        AccessError::from_sqlx("query purchase_order", &source, AllowedConstraints::NONE)
    })?;
    finish_page(&mut rows, prepared.page_limit, input.sort)
}

/// Apply one optimistic update without retrying serialization failures.
pub async fn update(
    connection: &mut WamnConnection,
    id: &str,
    expected_revision: i64,
    supplier_id: SupplierIdUpdate,
) -> Result<PurchaseOrderRow, AccessError> {
    let id = canonical_uuid(id, "purchase_order id")?;
    let (supplier_id_present, supplier_id) = supplier_update(supplier_id)?;
    let row = generated::update(
        connection,
        WamnUuid(id),
        expected_revision,
        supplier_id_present,
        supplier_id,
    )
    .await
    .map_err(|source| {
        AccessError::from_sqlx("update purchase_order", &source, UPDATE_CONSTRAINTS)
    })?;
    update_result(row)
}

#[derive(Debug)]
struct PreparedQuery {
    cursor: QueryCursor,
    supplier_ids: Option<Json>,
    statuses: Option<Json>,
    page_limit: usize,
    fetch_limit: i64,
}

#[derive(Debug)]
enum QueryCursor {
    Text(Option<DecodedCursor<Box<str>>>),
    Timestamp(Option<DecodedCursor<DateTime<Utc>>>),
}

fn prepare_query(input: &QueryInput) -> Result<PreparedQuery, AccessError> {
    let cursor = decode_query_cursor(input.sort, input.cursor.as_deref())?;
    let limit = page_limit(input.limit)?;
    let supplier_ids = supplier_filter(input.supplier_ids.as_deref())?;
    let statuses = status_filter(input.statuses.as_deref());
    Ok(PreparedQuery {
        cursor,
        supplier_ids,
        statuses,
        page_limit: usize::try_from(limit).expect("validated page limit fits usize"),
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

fn page_limit(limit: Option<i64>) -> Result<i64, AccessError> {
    let limit = limit.unwrap_or(MAX_PAGE_SIZE);
    if (1..=MAX_PAGE_SIZE).contains(&limit) {
        Ok(limit)
    } else {
        Err(AccessError::invalid("purchase_order limit must be 1..=100"))
    }
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
                .map(|value| canonical_uuid(value, "purchase_order supplier filter"))
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

fn supplier_update(supplier_id: SupplierIdUpdate) -> Result<(bool, Option<WamnUuid>), AccessError> {
    match supplier_id {
        SupplierIdUpdate::Omitted => Ok((false, None)),
        SupplierIdUpdate::Null => Err(AccessError::invalid(
            "purchase_order supplier_id does not accept explicit null",
        )),
        SupplierIdUpdate::Value(value) => Ok((
            true,
            Some(WamnUuid(canonical_uuid(
                &value,
                "purchase_order supplier_id",
            )?)),
        )),
    }
}

fn finish_page(
    rows: &mut Vec<PurchaseOrderRow>,
    page_limit: usize,
    sort: PurchaseOrderSort,
) -> Result<Page, AccessError> {
    let has_more = rows.len() > page_limit;
    rows.truncate(page_limit);
    let next_cursor = if has_more {
        Some(encode_row_cursor(
            rows.last().expect("a positive page limit retained one row"),
            sort,
        )?)
    } else {
        None
    };
    Ok(Page {
        item: std::mem::take(rows).into_boxed_slice(),
        next_cursor,
    })
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

fn canonical_uuid(value: &str, context: &str) -> Result<String, AccessError> {
    uuid::Uuid::parse_str(value)
        .ok()
        .filter(|parsed| parsed.hyphenated().to_string() == value)
        .map(|parsed| parsed.hyphenated().to_string())
        .ok_or_else(|| AccessError::invalid(format!("{context} is not a canonical UUID")))
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

fn update_result(row: generated::PurchaseOrderUpdateRow) -> Result<PurchaseOrderRow, AccessError> {
    match row.outcome.as_deref() {
        Some("not_found") => Err(AccessError::not_found("purchase_order does not exist")),
        Some("concurrency_conflict") => Err(AccessError::concurrency_conflict(
            "purchase_order revision does not match",
        )),
        Some("updated") => match (
            row.id,
            row.purchase_order_number,
            row.supplier_id,
            row.status,
            row.row_version,
            row.created_at,
            row.updated_at,
        ) {
            (
                Some(id),
                Some(purchase_order_number),
                Some(supplier_id),
                Some(status),
                Some(row_version),
                Some(created_at),
                Some(updated_at),
            ) => Ok(PurchaseOrderRow {
                created_at,
                id,
                purchase_order_number,
                row_version,
                status,
                supplier_id,
                updated_at,
            }),
            _ => Err(AccessError::internal(
                "purchase_order update returned an incomplete row",
            )),
        },
        _ => Err(AccessError::internal(
            "purchase_order update returned an unknown outcome",
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
    fn omitted_limit_is_one_hundred_and_out_of_range_is_invalid() {
        assert_eq!(page_limit(None).unwrap(), 100);
        assert_eq!(page_limit(Some(1)).unwrap(), 1);
        assert_eq!(page_limit(Some(100)).unwrap(), 100);
        for limit in [-1, 0, 101] {
            assert_eq!(
                page_limit(Some(limit)).unwrap_err().kind(),
                AccessErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn cursor_limit_and_filter_refusals_are_typed() {
        let input = QueryInput {
            supplier_ids: Some(vec!["not-a-uuid".into()].into_boxed_slice()),
            statuses: None,
            sort: PurchaseOrderSort::CreatedAtAscending,
            cursor: Some("not-base64".into()),
            limit: Some(0),
        };
        assert_eq!(
            prepare_query(&input).unwrap_err().kind(),
            AccessErrorKind::InvalidInput
        );
        assert_eq!(
            page_limit(Some(0)).unwrap_err().kind(),
            AccessErrorKind::InvalidInput
        );
        assert_eq!(
            supplier_filter(Some(&["not-a-uuid".into()]))
                .unwrap_err()
                .kind(),
            AccessErrorKind::InvalidInput
        );
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
    fn extra_row_mints_cursor_from_last_returned_row_and_normalizes_utc() {
        let mut rows = vec![
            row(FIRST_ID, "2026-08-29T12:34:56.123456+00:00"),
            row(SECOND_ID, "2026-08-29T12:35:56.123456+00:00"),
        ];
        let page = finish_page(&mut rows, 1, PurchaseOrderSort::CreatedAtAscending).unwrap();

        assert_eq!(page.item.len(), 1);
        assert_eq!(page.item[0].id.0, FIRST_ID);
        let cursor = decode_cursor::<DateTime<Utc>>(
            page.next_cursor.as_deref().unwrap(),
            "created_at",
            CursorDirection::Ascending,
        )
        .unwrap();
        assert_eq!(cursor.id.hyphenated().to_string(), FIRST_ID);
        assert_eq!(cursor.key.timestamp_subsec_micros(), 123_456);
        assert_eq!(
            canonical_timestamp(&cursor.key),
            "2026-08-29T12:34:56.123456Z"
        );
    }

    #[test]
    fn no_extra_row_has_no_next_cursor() {
        let mut rows = vec![row(FIRST_ID, "2026-08-29T12:34:56.123456+00:00")];
        let page = finish_page(&mut rows, 1, PurchaseOrderSort::CreatedAtAscending).unwrap();

        assert_eq!(page.item.len(), 1);
        assert!(page.next_cursor.is_none());
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
                update_result(update_row(outcome, false))
                    .unwrap_err()
                    .kind(),
                expected
            );
        }
        assert_eq!(
            update_result(update_row("updated", false))
                .unwrap_err()
                .kind(),
            AccessErrorKind::InternalError
        );
        let updated = update_result(update_row("updated", true)).unwrap();
        assert_eq!(updated.id.0, FIRST_ID);
        assert_eq!(updated.supplier_id.0, SECOND_ID);
        assert_eq!(updated.row_version, 2);
    }

    fn row(id: &str, created_at: &str) -> PurchaseOrderRow {
        PurchaseOrderRow {
            created_at: TimestampTz(created_at.to_owned()),
            id: WamnUuid(id.to_owned()),
            purchase_order_number: "PO-100".to_owned(),
            row_version: 1,
            status: "open".to_owned(),
            supplier_id: WamnUuid(SECOND_ID.to_owned()),
            updated_at: TimestampTz(created_at.to_owned()),
        }
    }

    fn update_row(outcome: &str, complete: bool) -> generated::PurchaseOrderUpdateRow {
        generated::PurchaseOrderUpdateRow {
            outcome: Some(outcome.to_owned()),
            created_at: complete.then(|| TimestampTz("2026-08-29T12:34:56+00:00".to_owned())),
            id: complete.then(|| WamnUuid(FIRST_ID.to_owned())),
            purchase_order_number: complete.then(|| "PO-100".to_owned()),
            row_version: complete.then_some(2),
            status: complete.then(|| "open".to_owned()),
            supplier_id: complete.then(|| WamnUuid(SECOND_ID.to_owned())),
            updated_at: complete.then(|| TimestampTz("2026-08-29T12:35:56+00:00".to_owned())),
        }
    }
}

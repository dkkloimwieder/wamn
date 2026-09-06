//! `pallet.get` and `pallet.query` -- the model reads over the generated
//! projection, keyset-paged the way `contracts/cursor-v1.json` fixes: one
//! generated statement per (sort field, direction), the tie-breaker on `id`,
//! and a cursor minted from the last row returned.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use wamn_postgres_statements::{Connection, Json, TimestampTz, Uuid};

use crate::cursor::{self, CursorDirection, CursorKey, DecodedCursor};
use crate::error::{self, AccessError, AccessErrorKind};
use crate::generated::wamn::pallet as sql;
use crate::scalar;

pub(crate) const GET_REFUSALS: &[AccessErrorKind] = &[
    AccessErrorKind::InvalidInput,
    AccessErrorKind::NotFound,
    AccessErrorKind::Retry,
    AccessErrorKind::Timeout,
    AccessErrorKind::PermissionDenied,
    AccessErrorKind::InternalError,
];

pub(crate) const QUERY_REFUSALS: &[AccessErrorKind] = &[
    AccessErrorKind::InvalidInput,
    AccessErrorKind::Retry,
    AccessErrorKind::Timeout,
    AccessErrorKind::PermissionDenied,
    AccessErrorKind::InternalError,
];

const MAX_PAGE_SIZE: i64 = 100;
const STATUSES: [&str; 3] = ["available", "held", "consumed"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetInput {
    pub(crate) id: String,
}

/// The query body: every member optional, the manifest's default sort when
/// none is named, and nothing the manifest did not declare.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QueryInput {
    #[serde(default)]
    filter: Option<Filter>,
    #[serde(default)]
    sort: Option<Sort>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Filter {
    #[serde(default)]
    status: Option<Vec<String>>,
    #[serde(default)]
    location_id: Option<Vec<String>>,
    #[serde(default)]
    pallet_code: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sort {
    field: SortField,
    direction: CursorDirection,
}

const DEFAULT_SORT: Sort = Sort {
    field: SortField::CreatedAt,
    direction: CursorDirection::Ascending,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum SortField {
    PalletCode,
    LocationId,
    UpdatedAt,
    CreatedAt,
}

impl SortField {
    const fn name(self) -> &'static str {
        match self {
            Self::PalletCode => "pallet_code",
            Self::LocationId => "location_id",
            Self::UpdatedAt => "updated_at",
            Self::CreatedAt => "created_at",
        }
    }
}

/// One bounded page and the cursor that continues it, if anything does.
#[derive(Debug)]
pub(crate) struct Page {
    pub(crate) item: Vec<sql::PalletRow>,
    pub(crate) next_cursor: Option<String>,
}

/// Load one pallet by id.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub(crate) async fn get(
    connection: &mut Connection,
    id: &str,
) -> Result<sql::PalletRow, AccessError> {
    let id = scalar::uuid("id", id)?;
    sql::get(connection, id.clone())
        .await
        .map_err(|e| error::from_statement(&e))?
        .ok_or_else(|| AccessError::missing(AccessErrorKind::NotFound, "id", &id.0))
}

/// The cursor's bindings, typed by the sort field it was minted under.
#[derive(Debug)]
enum Cursor {
    Text(Option<String>, Option<Uuid>),
    Id(Option<Uuid>, Option<Uuid>),
    Time(Option<TimestampTz>, Option<Uuid>),
}

/// Query one bounded page.
///
/// # Errors
///
/// [`AccessError`] carrying the literal the operation contract declares.
pub(crate) async fn query(
    connection: &mut Connection,
    input: &QueryInput,
) -> Result<Page, AccessError> {
    let sort = input.sort.unwrap_or(DEFAULT_SORT);
    // VALIDATION ORDER IS THE CONTRACT'S: cursor, then limit, then SQL.
    let cursor = decode(sort, input.cursor.as_deref())?;
    let limit = page_limit(input.limit)?;
    let filter = input.filter.as_ref();
    let statuses = status_filter(filter.and_then(|filter| filter.status.as_deref()))?;
    let locations = uuid_filter(filter.and_then(|filter| filter.location_id.as_deref()))?;
    let codes = filter
        .and_then(|filter| filter.pallet_code.as_deref())
        .map(text_json);
    // One row past the page tells whether a next page exists.
    let fetch = limit + 1;
    let mut rows = match (sort.field, sort.direction, cursor) {
        (SortField::PalletCode, CursorDirection::Ascending, Cursor::Text(key, id)) => {
            sql::query_pallet_code_ascending(connection, statuses, locations, codes, key, id, fetch)
                .await
        }
        (SortField::PalletCode, CursorDirection::Descending, Cursor::Text(key, id)) => {
            sql::query_pallet_code_descending(
                connection, statuses, locations, codes, key, id, fetch,
            )
            .await
        }
        (SortField::LocationId, CursorDirection::Ascending, Cursor::Id(key, id)) => {
            sql::query_location_id_ascending(connection, statuses, locations, codes, key, id, fetch)
                .await
        }
        (SortField::LocationId, CursorDirection::Descending, Cursor::Id(key, id)) => {
            sql::query_location_id_descending(
                connection, statuses, locations, codes, key, id, fetch,
            )
            .await
        }
        (SortField::UpdatedAt, CursorDirection::Ascending, Cursor::Time(key, id)) => {
            sql::query_updated_at_ascending(connection, statuses, locations, codes, key, id, fetch)
                .await
        }
        (SortField::UpdatedAt, CursorDirection::Descending, Cursor::Time(key, id)) => {
            sql::query_updated_at_descending(connection, statuses, locations, codes, key, id, fetch)
                .await
        }
        (SortField::CreatedAt, CursorDirection::Ascending, Cursor::Time(key, id)) => {
            sql::query_created_at_ascending(connection, statuses, locations, codes, key, id, fetch)
                .await
        }
        (SortField::CreatedAt, CursorDirection::Descending, Cursor::Time(key, id)) => {
            sql::query_created_at_descending(connection, statuses, locations, codes, key, id, fetch)
                .await
        }
        _ => unreachable!("the cursor was decoded for this sort field"),
    }
    .map_err(|e| error::from_statement(&e))?;
    finish_page(&mut rows, limit, sort)
}

fn decode(sort: Sort, encoded: Option<&str>) -> Result<Cursor, AccessError> {
    fn decoded<Key: CursorKey>(
        encoded: Option<&str>,
        sort: Sort,
    ) -> Result<Option<DecodedCursor<Key>>, AccessError> {
        encoded
            .map(|encoded| cursor::decode_cursor(encoded, sort.field.name(), sort.direction))
            .transpose()
    }
    let wire = |id: uuid::Uuid| Uuid(id.hyphenated().to_string());
    Ok(match sort.field {
        SortField::PalletCode => {
            let cursor = decoded::<String>(encoded, sort)?;
            Cursor::Text(
                cursor.as_ref().map(|cursor| cursor.key.clone()),
                cursor.map(|cursor| wire(cursor.id)),
            )
        }
        SortField::LocationId => {
            let cursor = decoded::<uuid::Uuid>(encoded, sort)?;
            Cursor::Id(
                cursor.as_ref().map(|cursor| wire(cursor.key)),
                cursor.map(|cursor| wire(cursor.id)),
            )
        }
        SortField::UpdatedAt | SortField::CreatedAt => {
            let cursor = decoded::<DateTime<Utc>>(encoded, sort)?;
            Cursor::Time(
                cursor
                    .as_ref()
                    .map(|cursor| TimestampTz(cursor::canonical_timestamp(&cursor.key))),
                cursor.map(|cursor| wire(cursor.id)),
            )
        }
    })
}

fn page_limit(limit: Option<i64>) -> Result<i64, AccessError> {
    let limit = limit.unwrap_or(MAX_PAGE_SIZE);
    if (1..=MAX_PAGE_SIZE).contains(&limit) {
        Ok(limit)
    } else {
        Err(AccessError::range("limit", 1, MAX_PAGE_SIZE, limit))
    }
}

fn status_filter(values: Option<&[String]>) -> Result<Option<Json>, AccessError> {
    values
        .map(|values| {
            if values
                .iter()
                .all(|value| STATUSES.contains(&value.as_str()))
            {
                Ok(text_json(values))
            } else {
                Err(AccessError::field(
                    AccessErrorKind::InvalidInput,
                    "filter.status",
                ))
            }
        })
        .transpose()
}

fn uuid_filter(values: Option<&[String]>) -> Result<Option<Json>, AccessError> {
    values
        .map(|values| {
            values
                .iter()
                .map(|value| scalar::uuid("filter.location_id", value).map(|uuid| uuid.0))
                .collect::<Result<Vec<_>, _>>()
                .map(|values| text_json(&values))
        })
        .transpose()
}

fn text_json(values: &[String]) -> Json {
    Json(serde_json::to_string(values).expect("strings serialize"))
}

fn finish_page(
    rows: &mut Vec<sql::PalletRow>,
    limit: i64,
    sort: Sort,
) -> Result<Page, AccessError> {
    let limit = usize::try_from(limit).expect("a validated page limit fits usize");
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if has_more {
        Some(row_cursor(
            rows.last().expect("a positive page limit retained one row"),
            sort,
        )?)
    } else {
        None
    };
    Ok(Page {
        item: std::mem::take(rows),
        next_cursor,
    })
}

/// A row that cannot mint a cursor is a deployment fault -- the database
/// answered with something outside its own types -- so it is opaque.
fn internal() -> AccessError {
    AccessError::new(AccessErrorKind::InternalError, json!({}))
}

fn row_cursor(row: &sql::PalletRow, sort: Sort) -> Result<String, AccessError> {
    let id = uuid::Uuid::parse_str(&row.id.0).map_err(|_| internal())?;
    let field = sort.field.name();
    Ok(match sort.field {
        SortField::PalletCode => cursor::encode_cursor(field, sort.direction, &row.pallet_code, id),
        SortField::LocationId => {
            let key = uuid::Uuid::parse_str(&row.location_id.0).map_err(|_| internal())?;
            cursor::encode_cursor(field, sort.direction, &key, id)
        }
        SortField::UpdatedAt => {
            cursor::encode_cursor(field, sort.direction, &row_time(&row.updated_at)?, id)
        }
        SortField::CreatedAt => {
            cursor::encode_cursor(field, sort.direction, &row_time(&row.created_at)?, id)
        }
    })
}

fn row_time(value: &TimestampTz) -> Result<DateTime<Utc>, AccessError> {
    DateTime::parse_from_rfc3339(&value.0)
        .map(|timestamp| timestamp.to_utc())
        .map_err(|_| internal())
}

/// The `pallet.get` result and each `pallet.query` item: the projection's
/// fields, `row_version` as the integer the contract types it.
pub(crate) fn row_to_json(row: &sql::PalletRow) -> Value {
    json!({
        "created_at": row.created_at.0,
        "id": row.id.0,
        "location_id": row.location_id.0,
        "pallet_code": row.pallet_code,
        "row_version": row.row_version,
        "status": row.status,
        "updated_at": row.updated_at.0,
    })
}

pub(crate) fn page_to_json(page: &Page) -> Value {
    json!({
        "item": page.item.iter().map(row_to_json).collect::<Vec<_>>(),
        "next_cursor": page.next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: &str = "01234567-89ab-cdef-0123-456789abcdef";
    const SECOND: &str = "11234567-89ab-cdef-0123-456789abcdef";
    const LOCATION: &str = "21234567-89ab-cdef-0123-456789abcdef";

    fn row(id: &str, created_at: &str) -> sql::PalletRow {
        sql::PalletRow {
            created_at: TimestampTz(created_at.to_owned()),
            id: Uuid(id.to_owned()),
            location_id: Uuid(LOCATION.to_owned()),
            pallet_code: "PAL-1".to_owned(),
            row_version: 1,
            status: "available".to_owned(),
            updated_at: TimestampTz(created_at.to_owned()),
        }
    }

    #[test]
    fn an_omitted_limit_is_one_hundred_and_out_of_range_carries_the_bounds() {
        assert_eq!(page_limit(None).unwrap(), 100);
        assert_eq!(page_limit(Some(1)).unwrap(), 1);
        for limit in [0, 101, -5] {
            let error = page_limit(Some(limit)).unwrap_err();
            assert_eq!(error.kind(), AccessErrorKind::InvalidInput);
            assert_eq!(error.detail()["observed"], limit);
            assert_eq!(error.detail()["maximum"], 100);
        }
    }

    #[test]
    fn the_extra_row_mints_the_cursor_from_the_last_row_kept() {
        let mut rows = vec![
            row(FIRST, "2026-08-29T12:34:56.123456+00:00"),
            row(SECOND, "2026-08-29T12:35:56.123456+00:00"),
        ];
        let page = finish_page(&mut rows, 1, DEFAULT_SORT).unwrap();
        assert_eq!(page.item.len(), 1);
        assert_eq!(page.item[0].id.0, FIRST);
        let Cursor::Time(Some(key), Some(id)) =
            decode(DEFAULT_SORT, page.next_cursor.as_deref()).unwrap()
        else {
            panic!("a created_at cursor binds a timestamp and an id");
        };
        assert_eq!(key.0, "2026-08-29T12:34:56.123456Z");
        assert_eq!(id.0, FIRST);

        let mut rows = vec![row(FIRST, "2026-08-29T12:34:56.123456+00:00")];
        assert!(
            finish_page(&mut rows, 1, DEFAULT_SORT)
                .unwrap()
                .next_cursor
                .is_none()
        );
    }

    #[test]
    fn a_cursor_is_bound_to_the_sort_that_minted_it() {
        let location_sort = Sort {
            field: SortField::LocationId,
            direction: CursorDirection::Descending,
        };
        let encoded = row_cursor(&row(FIRST, "2026-08-29T12:34:56+00:00"), location_sort).unwrap();
        assert!(matches!(
            decode(location_sort, Some(&encoded)).unwrap(),
            Cursor::Id(Some(_), Some(_))
        ));
        assert_eq!(
            decode(DEFAULT_SORT, Some(&encoded)).unwrap_err().kind(),
            AccessErrorKind::InvalidInput
        );
        assert!(matches!(
            decode(DEFAULT_SORT, None).unwrap(),
            Cursor::Time(None, None)
        ));
    }

    #[test]
    fn filters_are_validated_and_bound_as_json_arrays() {
        assert_eq!(
            status_filter(Some(&["held".to_owned()]))
                .unwrap()
                .unwrap()
                .0,
            r#"["held"]"#
        );
        assert!(status_filter(Some(&["missing".to_owned()])).is_err());
        assert_eq!(
            uuid_filter(Some(&[LOCATION.to_uppercase()]))
                .unwrap()
                .unwrap()
                .0,
            format!("[\"{LOCATION}\"]")
        );
        assert!(uuid_filter(Some(&["nope".to_owned()])).is_err());
        assert!(status_filter(None).unwrap().is_none());
    }

    #[test]
    fn the_query_body_admits_only_declared_members() {
        let parsed: QueryInput = serde_json::from_value(json!({
            "filter": {"status": ["available"]},
            "sort": {"field": "pallet_code", "direction": "descending"},
            "limit": 5
        }))
        .unwrap();
        assert_eq!(parsed.sort.unwrap().field, SortField::PalletCode);
        assert!(serde_json::from_value::<QueryInput>(json!({"offset": 3})).is_err());
        assert!(
            serde_json::from_value::<QueryInput>(
                json!({"sort": {"field": "status", "direction": "ascending"}})
            )
            .is_err()
        );
        assert!(serde_json::from_value::<GetInput>(json!({"id": FIRST, "extra": 1})).is_err());
    }

    #[test]
    fn the_page_serializes_with_integer_revisions() {
        let page = Page {
            item: vec![row(FIRST, "2026-08-29T12:34:56.123456Z")],
            next_cursor: None,
        };
        let value = page_to_json(&page);
        assert_eq!(value["item"][0]["row_version"], 1);
        assert_eq!(value["item"][0]["id"], FIRST);
        assert!(value["next_cursor"].is_null());
    }
}

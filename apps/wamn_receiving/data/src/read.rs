//! Application-owned read transactions and history image projection.

use crate::cursor::{CursorDirection, decode_cursor, encode_cursor};
use crate::error::{AccessError, AllowedConstraints};
use crate::generated::wamn::{
    location_list as location_sql, receiving_load_purchase_order_history as history_sql,
    receiving_load_receipt_screen as screen_sql,
};
use wamn_postgres_statements::{Connection, Uuid};

pub use location_sql::ListLocationsRow;
pub use screen_sql::LoadReceiptScreenRow;

/// The largest page of one purchase order history read.
const MAX_HISTORY_PAGE: i32 = 100;
/// The sort field that the history cursor names.
const HISTORY_CURSOR_FIELD: &str = "position";
/// The history reads forward, from the oldest entry to the newest.
const HISTORY_CURSOR_DIRECTION: CursorDirection = CursorDirection::Ascending;
/// The `purchase_order` columns that `receiving.load_purchase_order_history`
/// declares. Every image that the read returns keeps only these columns.
const PURCHASE_ORDER_HISTORY_COLUMNS: [&str; 9] = [
    "id",
    "purchase_order_number",
    "supplier_id",
    "status",
    "row_version",
    "created_at",
    "created_by",
    "updated_at",
    "updated_by",
];

/// One history entry, with the opaque cursor that reads the page after it.
///
/// The database position stays inside this module. A caller pages by sending
/// back the cursor of the last entry it read.
#[derive(Debug)]
pub struct PurchaseOrderHistoryValue {
    pub id: Uuid,
    pub cursor: String,
    pub kind: String,
    pub operation: String,
    pub changed_by: Uuid,
    pub changed_at: wamn_postgres_statements::TimestampTz,
    pub before: String,
    pub after: String,
    pub current: String,
}

fn history_value(
    row: history_sql::LoadPurchaseOrderHistoryRow,
    id: &Uuid,
) -> Result<PurchaseOrderHistoryValue, AccessError> {
    let image = |text: Option<String>| {
        text.and_then(|text| {
            wamn_record_history::retain_columns(&text, &PURCHASE_ORDER_HISTORY_COLUMNS)
        })
        .ok_or_else(|| AccessError::internal("history read returned no JSON object image"))
    };
    let record = parse_history_id(id)?;
    Ok(PurchaseOrderHistoryValue {
        id: row.id,
        cursor: encode_cursor(
            HISTORY_CURSOR_FIELD,
            HISTORY_CURSOR_DIRECTION,
            &row.position,
            record,
        )?,
        kind: row.kind,
        operation: row.operation,
        changed_by: row.changed_by,
        changed_at: row.changed_at,
        before: image(row.before)?,
        after: image(row.after)?,
        current: image(row.current)?,
    })
}

/// Read the declared locations in one transaction.
pub async fn location_list(
    connection: &mut Connection,
) -> Result<Vec<ListLocationsRow>, AccessError> {
    let mut transaction = connection.begin().await.map_err(|source| {
        AccessError::from_statement("begin location list", &source, AllowedConstraints::NONE)
    })?;
    let rows = location_sql::list_locations(&mut transaction)
        .await
        .map_err(|source| {
            AccessError::from_statement("list locations", &source, AllowedConstraints::NONE)
        })?;
    transaction.commit().await.map_err(|source| {
        AccessError::from_statement("commit location list", &source, AllowedConstraints::NONE)
    })?;
    Ok(rows)
}

/// Read the purchase order and its lines in one transaction.
pub async fn receipt_screen(
    connection: &mut Connection,
    id: Uuid,
) -> Result<Vec<LoadReceiptScreenRow>, AccessError> {
    let mut transaction = connection.begin().await.map_err(|source| {
        AccessError::from_statement(
            "begin receipt screen load",
            &source,
            AllowedConstraints::NONE,
        )
    })?;
    let rows = screen_sql::load_receipt_screen(&mut transaction, id)
        .await
        .map_err(|source| {
            AccessError::from_statement("load receipt screen", &source, AllowedConstraints::NONE)
        })?;
    transaction.commit().await.map_err(|source| {
        AccessError::from_statement(
            "commit receipt screen load",
            &source,
            AllowedConstraints::NONE,
        )
    })?;
    if rows.is_empty() {
        return Err(AccessError::not_found("purchase_order does not exist"));
    }
    Ok(rows)
}

/// Read retained history and project the declared purchase-order image.
pub async fn purchase_order_history(
    connection: &mut Connection,
    id: Uuid,
    after_cursor: Option<&str>,
    limit: i32,
) -> Result<Vec<PurchaseOrderHistoryValue>, AccessError> {
    if !(1..=MAX_HISTORY_PAGE).contains(&limit) {
        return Err(AccessError::invalid(
            "history limit is outside the supported page",
            "limit",
        ));
    }
    let after_position = history_position(after_cursor, &id)?;
    let mut transaction = connection.begin().await.map_err(|source| {
        AccessError::from_statement(
            "begin purchase order history load",
            &source,
            AllowedConstraints::NONE,
        )
    })?;
    let rows = history_sql::load_purchase_order_history(
        &mut transaction,
        id.clone(),
        after_position,
        limit,
    )
    .await
    .map_err(|source| {
        AccessError::from_statement(
            "load purchase order history",
            &source,
            AllowedConstraints::NONE,
        )
    })?;
    transaction.commit().await.map_err(|source| {
        AccessError::from_statement(
            "commit purchase order history load",
            &source,
            AllowedConstraints::NONE,
        )
    })?;
    rows.into_iter()
        .map(|row| history_value(row, &id))
        .collect()
}

/// The position that one opaque cursor names, or the start of the history.
///
/// A cursor that names another record refuses, because its position belongs to
/// that record's history.
fn history_position(after_cursor: Option<&str>, id: &Uuid) -> Result<i64, AccessError> {
    let Some(encoded) = after_cursor.filter(|cursor| !cursor.is_empty()) else {
        return Ok(0);
    };
    let record = parse_history_id(id)?;
    let decoded = decode_cursor::<i64>(encoded, HISTORY_CURSOR_FIELD, HISTORY_CURSOR_DIRECTION)?;
    if decoded.id != record {
        return Err(AccessError::invalid(
            "cursor names another purchase order",
            "after_cursor",
        ));
    }
    Ok(decoded.key)
}

fn parse_history_id(id: &Uuid) -> Result<uuid::Uuid, AccessError> {
    uuid::Uuid::parse_str(&id.0)
        .ok()
        .ok_or_else(|| AccessError::invalid("id is not a UUID", "id"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AccessErrorKind;
    use serde_json::Value;
    use wamn_postgres_statements::TimestampTz;

    #[test]
    fn history_columns_are_the_declared_purchase_order_columns() {
        let manifest: Value = serde_json::from_str(include_str!("../../wamn.json")).unwrap();
        let declared =
            manifest["custom_operations"]["receiving.load_purchase_order_history"]["relations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|relation| relation["table"] == "purchase_order")
                .unwrap()["select_fields"]
                .clone();
        assert_eq!(declared, serde_json::json!(PURCHASE_ORDER_HISTORY_COLUMNS));
    }

    #[test]
    fn history_rows_keep_declared_columns_and_carry_an_opaque_cursor() {
        let record = Uuid("00000000-0000-0000-0000-000000000001".to_owned());
        let row = |current: Option<&str>| history_sql::LoadPurchaseOrderHistoryRow {
            id: Uuid("00000000-0000-0000-0000-00000000000e".to_owned()),
            position: 2,
            kind: "update".to_owned(),
            operation: "client-acme-receiving:purchase-order/update@3.0.0".to_owned(),
            changed_by: Uuid("00000000-0000-0000-0000-000000000004".to_owned()),
            changed_at: TimestampTz("2026-08-31T12:01:00.000000Z".to_owned()),
            before: Some(r#"{"row_version": 1, "acme_quality_status": "pending"}"#.to_owned()),
            after: Some(r#"{"row_version": 2, "acme_quality_status": "approved"}"#.to_owned()),
            current: current.map(str::to_owned),
        };
        let value = history_value(
            row(Some(
                r#"{"id": "00000000-0000-0000-0000-000000000001", "row_version": 2, "acme_inspection_required": true}"#,
            )),
            &record,
        )
        .unwrap();
        assert_eq!(value.id.0, "00000000-0000-0000-0000-00000000000e");
        assert_eq!(value.kind, "update");
        assert_eq!(
            value.operation,
            "client-acme-receiving:purchase-order/update@3.0.0"
        );
        assert_eq!(value.changed_by.0, "00000000-0000-0000-0000-000000000004");
        assert_eq!(value.changed_at.0, "2026-08-31T12:01:00.000000Z");
        assert_eq!(value.before, r#"{"row_version": 1}"#);
        assert_eq!(value.after, r#"{"row_version": 2}"#);
        assert_eq!(
            value.current,
            r#"{"id": "00000000-0000-0000-0000-000000000001", "row_version": 2}"#
        );

        // The cursor carries no readable position, and it reads back as the
        // position of the entry it followed.
        assert_ne!(value.cursor, "2");
        assert_eq!(
            history_position(Some(&value.cursor), &record).unwrap(),
            2,
            "a cursor reads back as the position it names"
        );
        // A deleted row keeps the empty image.
        assert_eq!(
            history_value(row(Some("{}")), &record).unwrap().current,
            "{}"
        );

        for current in [None, Some("[]")] {
            let error = history_value(row(current), &record)
                .expect_err("a null or malformed image refuses");
            assert_eq!(error.kind(), AccessErrorKind::InternalError);
        }
    }

    #[test]
    fn a_history_cursor_belongs_to_one_record_and_starts_at_the_beginning() {
        let record = Uuid("00000000-0000-0000-0000-000000000001".to_owned());
        let other = Uuid("00000000-0000-0000-0000-000000000002".to_owned());
        assert_eq!(history_position(None, &record).unwrap(), 0);
        assert_eq!(history_position(Some(""), &record).unwrap(), 0);

        let cursor = encode_cursor(
            HISTORY_CURSOR_FIELD,
            HISTORY_CURSOR_DIRECTION,
            &7_i64,
            uuid::Uuid::parse_str(&record.0).unwrap(),
        )
        .unwrap();
        assert_eq!(history_position(Some(&cursor), &record).unwrap(), 7);
        let error = history_position(Some(&cursor), &other)
            .expect_err("a cursor of another record refuses");
        assert_eq!(error.kind(), AccessErrorKind::InvalidInput);
        let error = history_position(Some("not-a-cursor"), &record)
            .expect_err("a malformed cursor refuses");
        assert_eq!(error.kind(), AccessErrorKind::InvalidInput);
    }
}

//! Application-owned read connections and history image projection.

use crate::cursor::{CursorDirection, decode_cursor, encode_cursor};
use crate::error::{AccessError, AllowedConstraints};
use crate::statements::wamn::{
    location_list as location_sql, receiving_load_purchase_order_history as history_sql,
    receiving_load_receipt_screen as screen_sql, receiving_purchase_order_summary as summary_sql,
};
use wamn_postgres_statements::{Connection, Numeric, Uuid};

pub use location_sql::ListLocationsRow;
pub use summary_sql::PurchaseOrderSummaryRow;

/// One purchase order line of the receipt screen, with its order and item.
#[derive(Debug)]
pub struct ReceiptScreenLine {
    pub purchase_order_id: Uuid,
    pub purchase_order_number: String,
    pub purchase_order_status: String,
    pub supplier_id: Uuid,
    pub row_version: i32,
    pub line_id: Uuid,
    pub line_number: i32,
    pub item_id: Uuid,
    pub item_number: String,
    pub ordered_quantity: Numeric,
    pub received_quantity: Numeric,
    pub remaining_quantity: Numeric,
}

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
    pub type_: String,
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
            wamn_record_history::retain_columns(
                &text,
                &PURCHASE_ORDER_HISTORY_COLUMNS,
                crate::generated::purchase_order::TIMESTAMPTZ_COLUMNS,
            )
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
        type_: row.type_,
        operation: row.operation,
        changed_by: row.changed_by,
        changed_at: row.changed_at,
        before: image(row.before)?,
        after: image(row.after)?,
        current: image(row.current)?,
    })
}

/// Read the declared locations through the connection.
pub async fn location_list(
    connection: &mut Connection,
) -> Result<Vec<ListLocationsRow>, AccessError> {
    let rows = location_sql::list_locations(connection)
        .await
        .map_err(|source| {
            AccessError::from_statement("list locations", &source, AllowedConstraints::NONE)
        })?;
    Ok(rows)
}

/// Read one receipt screen row per purchase order line.
///
/// A missing order is not found. An order with no lines reads as no rows.
pub async fn receipt_screen(
    connection: &mut Connection,
    id: Uuid,
) -> Result<Vec<ReceiptScreenLine>, AccessError> {
    let rows = screen_sql::load_receipt_screen(connection, id)
        .await
        .map_err(|source| {
            AccessError::from_statement("load receipt screen", &source, AllowedConstraints::NONE)
        })?;
    receipt_screen_lines(rows)
}

/// The statement anchors on the order, so no row means no order, and an order
/// with no lines comes back as one row with null line columns.
fn receipt_screen_lines(
    rows: Vec<screen_sql::LoadReceiptScreenRow>,
) -> Result<Vec<ReceiptScreenLine>, AccessError> {
    if rows.is_empty() {
        return Err(AccessError::not_found("purchase_order does not exist"));
    }
    rows.into_iter()
        .filter(|row| row.line_id.is_some())
        .map(|row| {
            let missing = || AccessError::internal("receipt screen line has a null column");
            Ok(ReceiptScreenLine {
                purchase_order_id: row.purchase_order_id,
                purchase_order_number: row.purchase_order_number,
                purchase_order_status: row.purchase_order_status,
                supplier_id: row.supplier_id,
                row_version: row.row_version,
                line_id: row.line_id.ok_or_else(missing)?,
                line_number: row.line_number.ok_or_else(missing)?,
                item_id: row.item_id.ok_or_else(missing)?,
                item_number: row.item_number.ok_or_else(missing)?,
                ordered_quantity: row.ordered_quantity.ok_or_else(missing)?,
                received_quantity: row.received_quantity.ok_or_else(missing)?,
                remaining_quantity: row.remaining_quantity.ok_or_else(missing)?,
            })
        })
        .collect()
}

/// Read the receiving totals for orders with the supplied supplier and status.
pub async fn purchase_order_summary(
    connection: &mut Connection,
    supplier_id: Uuid,
    status: String,
) -> Result<Vec<PurchaseOrderSummaryRow>, AccessError> {
    summary_sql::purchase_order_summary(connection, supplier_id, status)
        .await
        .map_err(|source| {
            AccessError::from_statement("purchase order summary", &source, AllowedConstraints::NONE)
        })
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
    let rows =
        history_sql::load_purchase_order_history(connection, id.clone(), after_position, limit)
            .await
            .map_err(|source| {
                AccessError::from_statement(
                    "load purchase order history",
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
    use crate::AccessErrorType;
    use serde_json::Value;
    use wamn_postgres_statements::TimestampTz;

    #[test]
    fn history_columns_are_the_declared_purchase_order_columns() {
        let manifest: Value = serde_json::from_str(include_str!(
            "../../../target/wamn/wamn_receiving/wamn.json"
        ))
        .unwrap();
        // Generation derives the relation from the statement, in name order.
        let declared =
            manifest["custom_operations"]["receiving.load_purchase_order_history"]["relations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|relation| relation["table"] == "purchase_order")
                .unwrap()["select_fields"]
                .clone();
        let mut columns = PURCHASE_ORDER_HISTORY_COLUMNS;
        columns.sort_unstable();
        assert_eq!(declared, serde_json::json!(columns));
    }

    #[test]
    fn history_rows_keep_declared_columns_and_carry_an_opaque_cursor() {
        let record = Uuid("00000000-0000-0000-0000-000000000001".to_owned());
        let row = |current: Option<&str>| history_sql::LoadPurchaseOrderHistoryRow {
            id: Uuid("00000000-0000-0000-0000-00000000000e".to_owned()),
            position: 2,
            type_: "update".to_owned(),
            operation: "client-acme-receiving:purchase-order/update@4.0.0".to_owned(),
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
        assert_eq!(value.type_, "update");
        assert_eq!(
            value.operation,
            "client-acme-receiving:purchase-order/update@4.0.0"
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
            assert_eq!(error.error_type(), AccessErrorType::InternalError);
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
        assert_eq!(error.error_type(), AccessErrorType::InvalidInput);
        let error = history_position(Some("not-a-cursor"), &record)
            .expect_err("a malformed cursor refuses");
        assert_eq!(error.error_type(), AccessErrorType::InvalidInput);
    }

    #[test]
    fn a_missing_order_is_not_found_and_an_order_without_lines_reads_empty() {
        let row = |line: Option<i32>| screen_sql::LoadReceiptScreenRow {
            purchase_order_id: Uuid("00000000-0000-0000-0000-000000000001".to_owned()),
            purchase_order_number: "PO-1".to_owned(),
            purchase_order_status: "open".to_owned(),
            supplier_id: Uuid("00000000-0000-0000-0000-000000000002".to_owned()),
            row_version: 1,
            line_id: line.map(|_| Uuid("00000000-0000-0000-0000-000000000003".to_owned())),
            line_number: line,
            item_id: line.map(|_| Uuid("00000000-0000-0000-0000-000000000004".to_owned())),
            item_number: line.map(|_| "ITEM-1".to_owned()),
            ordered_quantity: line.map(|_| Numeric("2".to_owned())),
            received_quantity: line.map(|_| Numeric("0".to_owned())),
            remaining_quantity: line.map(|_| Numeric("2".to_owned())),
        };
        let error = receipt_screen_lines(Vec::new()).expect_err("a missing order refuses");
        assert_eq!(error.error_type(), AccessErrorType::NotFound);
        assert!(receipt_screen_lines(vec![row(None)]).unwrap().is_empty());
        let lines = receipt_screen_lines(vec![row(Some(1)), row(Some(2))]).unwrap();
        assert_eq!(
            lines
                .iter()
                .map(|line| line.line_number)
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }
}

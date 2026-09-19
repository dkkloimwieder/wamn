//! Application-owned read transactions and history image projection.

use crate::error::{AccessError, AllowedConstraints};
use crate::generated::wamn::{
    location_list as location_sql, receiving_load_purchase_order_history as history_sql,
    receiving_load_receipt_screen as screen_sql,
};
use wamn_postgres_statements::{Connection, Uuid};

pub use location_sql::ListLocationsRow;
pub use screen_sql::LoadReceiptScreenRow;

/// The largest page of one purchase order history read.
const MAX_HISTORY_PAGE: i64 = 100;
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

#[derive(Debug)]
pub struct PurchaseOrderHistoryValue {
    pub position: i64,
    pub kind: String,
    pub operation: String,
    pub changed_by: Uuid,
    pub changed_at: wamn_postgres_statements::TimestampTz,
    pub transaction_id: i64,
    pub before: String,
    pub after: String,
    pub current: String,
    pub head_position: i64,
}

impl TryFrom<history_sql::LoadPurchaseOrderHistoryRow> for PurchaseOrderHistoryValue {
    type Error = AccessError;

    fn try_from(row: history_sql::LoadPurchaseOrderHistoryRow) -> Result<Self, AccessError> {
        let image = |text: Option<String>| {
            text.and_then(|text| {
                wamn_record_history::retain_columns(&text, &PURCHASE_ORDER_HISTORY_COLUMNS)
            })
            .ok_or_else(|| AccessError::internal("history read returned no JSON object image"))
        };
        Ok(Self {
            position: row.position,
            kind: row.kind,
            operation: row.operation,
            changed_by: row.changed_by,
            changed_at: row.changed_at,
            transaction_id: row.transaction_id,
            before: image(row.before)?,
            after: image(row.after)?,
            current: image(row.current)?,
            head_position: row
                .head_position
                .ok_or_else(|| AccessError::internal("history read returned no head position"))?,
        })
    }
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
    after_position: i64,
    limit: i64,
) -> Result<Vec<PurchaseOrderHistoryValue>, AccessError> {
    if !(1..=MAX_HISTORY_PAGE).contains(&limit) {
        return Err(AccessError::invalid(
            "history limit is outside the supported page",
            "limit",
        ));
    }
    let mut transaction = connection.begin().await.map_err(|source| {
        AccessError::from_statement(
            "begin purchase order history load",
            &source,
            AllowedConstraints::NONE,
        )
    })?;
    let rows =
        history_sql::load_purchase_order_history(&mut transaction, id, after_position, limit)
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
        .map(PurchaseOrderHistoryValue::try_from)
        .collect()
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
    fn history_rows_keep_declared_columns_with_raw_values() {
        let row = |current: Option<&str>, head_position: Option<i64>| {
            history_sql::LoadPurchaseOrderHistoryRow {
                position: 2,
                kind: "update".to_owned(),
                operation: "client-acme-receiving:purchase-order/update@3.0.0".to_owned(),
                changed_by: Uuid("00000000-0000-0000-0000-000000000004".to_owned()),
                changed_at: TimestampTz("2026-08-31T12:01:00.000000Z".to_owned()),
                transaction_id: 4_294_967_297,
                before: Some(r#"{"row_version": 1, "acme_quality_status": "pending"}"#.to_owned()),
                after: Some(r#"{"row_version": 2, "acme_quality_status": "approved"}"#.to_owned()),
                current: current.map(str::to_owned),
                head_position,
            }
        };
        let value = PurchaseOrderHistoryValue::try_from(row(
            Some(r#"{"id": "00000000-0000-0000-0000-000000000001", "row_version": 2, "acme_inspection_required": true}"#),
            Some(2),
        ))
        .unwrap();
        assert_eq!(value.position, 2);
        assert_eq!(value.transaction_id, 4_294_967_297);
        assert_eq!(value.head_position, 2);
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
        for (current, head_position) in [(None, Some(2)), (Some("[]"), Some(2)), (Some("{}"), None)]
        {
            let error = PurchaseOrderHistoryValue::try_from(row(current, head_position))
                .expect_err("a null or malformed field refuses");
            assert_eq!(error.kind(), AccessErrorKind::InternalError);
        }
    }
}

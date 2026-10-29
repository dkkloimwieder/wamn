//! Typed read handlers over the existing application operations.

use wamn_postgres_statements::{Connection, Uuid};
use wamn_receiving_data_access::{AccessError, AccessErrorType, read};

pub(super) fn error_detail(
    error: &AccessError,
    key: &str,
    operation: &str,
    id: Option<(&str, &str)>,
    expected: Option<i32>,
) -> Option<String> {
    match key {
        "field" if error.error_type() == AccessErrorType::NotFound => {
            id.map(|(field, _)| field.to_owned())
        }
        "field" => error.field().map(str::to_owned),
        "id" => id.map(|(_, id)| id.to_owned()),
        "operation" => Some(operation.to_owned()),
        "expected_row_version" => expected.map(|value| value.to_string()),
        "observed_row_version" => error.observed_row_version().map(|value| value.to_string()),
        "constraint" => error.constraint().map(str::to_owned),
        "minimum" => error.minimum().map(|value| value.to_string()),
        "maximum" => error.maximum().map(|value| value.to_string()),
        "observed" => error.observed().map(|value| value.to_string()),
        _ => None,
    }
}

pub(super) mod location_list {
    use super::{Connection, error_detail, read};
    use crate::exports::wamn_receiving::location::list as contract;
    include!("../../../target/wamn/wamn_receiving/wit/location_list_codec.rs");

    pub(super) async fn execute(
        connection: &mut Connection,
        _request: contract::ListRequest,
    ) -> Result<contract::ListResult, contract::ListError> {
        read::location_list(connection)
            .await
            .map(|rows| contract::ListResult {
                rows: rows
                    .into_iter()
                    .map(|value| row!(value, contract::ListRow))
                    .collect(),
            })
            .map_err(|error| {
                map_error(error.error_type().literal(), |key| {
                    error_detail(&error, key, "location.list", None, None)
                })
            })
    }
}

pub(super) mod receiving_load_purchase_order_history {
    use super::{Connection, Uuid, error_detail, read};
    use crate::exports::wamn_receiving::receiving::load_purchase_order_history as contract;
    include!(
        "../../../target/wamn/wamn_receiving/wit/receiving_load_purchase_order_history_codec.rs"
    );

    pub(super) async fn execute(
        connection: &mut Connection,
        request: contract::LoadPurchaseOrderHistoryRequest,
    ) -> Result<contract::LoadPurchaseOrderHistoryResult, contract::LoadPurchaseOrderHistoryError>
    {
        read::purchase_order_history(
            connection,
            Uuid(request.id.clone()),
            request.after_cursor.as_deref(),
            request.limit,
        )
        .await
        .map(|rows| contract::LoadPurchaseOrderHistoryResult {
            rows: rows
                .into_iter()
                .map(|value| row!(value, contract::LoadPurchaseOrderHistoryRow))
                .collect(),
        })
        .map_err(|error| {
            map_error(error.error_type().literal(), |key| {
                error_detail(
                    &error,
                    key,
                    "receiving.load_purchase_order_history",
                    None,
                    None,
                )
            })
        })
    }
}

pub(super) mod receiving_purchase_order_summary {
    use super::{Connection, Uuid, error_detail, read};
    use crate::exports::wamn_receiving::receiving::purchase_order_summary as contract;
    include!("../../../target/wamn/wamn_receiving/wit/receiving_purchase_order_summary_codec.rs");

    pub(super) async fn execute(
        connection: &mut Connection,
        request: contract::PurchaseOrderSummaryRequest,
    ) -> Result<contract::PurchaseOrderSummaryResult, contract::PurchaseOrderSummaryError> {
        read::purchase_order_summary(connection, Uuid(request.supplier_id), request.status)
            .await
            .map(|rows| contract::PurchaseOrderSummaryResult {
                rows: rows
                    .into_iter()
                    .map(|value| row!(value, contract::PurchaseOrderSummaryRow))
                    .collect(),
            })
            .map_err(|error| {
                map_error(error.error_type().literal(), |key| {
                    error_detail(&error, key, "receiving.purchase_order_summary", None, None)
                })
            })
    }
}

location_list::export_operation!(
    crate::Component,
    crate::exports::wamn_receiving::location::list,
    crate::wamn::node::types,
    Connection::new(),
    location_list::execute,
    location_list
);

receiving_load_purchase_order_history::export_operation!(
    crate::Component,
    crate::exports::wamn_receiving::receiving::load_purchase_order_history,
    crate::wamn::node::types,
    Connection::new(),
    receiving_load_purchase_order_history::execute,
    receiving_load_purchase_order_history
);

receiving_purchase_order_summary::export_operation!(
    crate::Component,
    crate::exports::wamn_receiving::receiving::purchase_order_summary,
    crate::wamn::node::types,
    Connection::new(),
    receiving_purchase_order_summary::execute,
    receiving_purchase_order_summary
);

#[cfg(test)]
mod tests {
    use super::{location_list as locations, receiving_purchase_order_summary as summary};
    use crate::exports::wamn_receiving::{
        location::list as location_contract,
        receiving::{
            load_receipt_screen as screen_contract, purchase_order_summary as summary_contract,
        },
    };
    use crate::generated::{
        purchase_order::get::codec as get, receiving_load_receipt_screen::codec as screen,
    };
    use wamn_postgres_statements::{Numeric, Uuid};

    #[test]
    fn a_read_request_element_carries_no_request_id() {
        let request_elements = get::decode(
            r#"[
            {"request_id":"first","id":"00000000-0000-0000-0000-000000000001"},
            {"id":"00000000-0000-0000-0000-000000000002"}
        ]"#,
        )
        .unwrap();
        assert!(
            request_elements[0].input.is_err(),
            "a read refuses a request identity"
        );
        assert!(request_elements[1].input.is_ok());
    }

    #[test]
    fn envelope_preserves_request_element_order() {
        let request_elements = get::decode(
            r#"[
            {"id":"00000000-0000-0000-0000-000000000002"},
            {"id":"00000000-0000-0000-0000-000000000001"}
        ]"#,
        )
        .unwrap();
        assert_eq!(
            request_elements[0].input.as_ref().unwrap().id,
            "00000000-0000-0000-0000-000000000002"
        );
        assert_eq!(
            request_elements[1].input.as_ref().unwrap().id,
            "00000000-0000-0000-0000-000000000001"
        );
    }

    #[test]
    fn dto_unknown_fields_and_non_int64_wire_scalars_refuse_in_memory() {
        assert!(
            crate::generated::purchase_order::query::codec::decode(r#"[{"limit":2}]"#)
                .unwrap()
                .is_ok()
        );
        assert!(
            get::decode(r#"[{"id":"00000000-0000-0000-0000-000000000001","future":true}]"#)
                .unwrap()[0]
                .input
                .is_err()
        );
        assert!(
            locations::decode(r#"[{"unexpected":true}]"#).unwrap()[0]
                .input
                .is_err()
        );
        for value in ["", "1.0", "9223372036854775808"] {
            let input = serde_json::json!([{"id":"00000000-0000-0000-0000-000000000001","after_position":value,"limit":1}]);
            assert!(
                super::receiving_load_purchase_order_history::decode(&input.to_string()).unwrap()
                    [0]
                .input
                .is_err()
            );
        }
    }

    #[test]
    fn missing_orders_and_orders_without_lines_encode_empty_pages() {
        for cursor in ["missing-order", "order-without-lines"] {
            let output = [screen_contract::LoadReceiptScreenOutcome {
                outcome: Ok(screen_contract::LoadReceiptScreenResult::Page(
                    screen_contract::LoadReceiptScreenPage {
                        item: vec![],
                        next_cursor: None,
                        cursor: cursor.to_owned(),
                    },
                )),
            }];
            let value: serde_json::Value = serde_json::from_str(&screen::encode(&output)).unwrap();
            assert_eq!(value[0]["value"]["item"], serde_json::json!([]));
            assert_eq!(value[0]["value"]["cursor"], cursor);
        }
    }

    #[test]
    fn bounded_projection_rows_preserve_the_declared_wire_scalars() {
        let row = wamn_receiving_data_access::read::ListLocationsRow {
            id: Uuid("00000000-0000-0000-0000-000000000001".to_owned()),
            location_code: "DOCK-A".to_owned(),
        };
        let output = [location_contract::ListOutcome {
            outcome: Ok(location_contract::ListResult {
                rows: vec![locations::row!(row, location_contract::ListRow)],
            }),
        }];
        let value: serde_json::Value = serde_json::from_str(&locations::encode(&output)).unwrap();
        assert_eq!(value[0]["value"]["rows"][0]["location_code"], "DOCK-A");
        assert!(
            value[0].get("request_id").is_none(),
            "a read outcome carries no request identity"
        );

        let row = wamn_receiving_data_access::generated::receiving_load_receipt_screen::Row {
            purchase_order_id: Uuid("00000000-0000-0000-0000-000000000002".to_owned()),
            purchase_order_number: "PO-1".to_owned(),
            purchase_order_status: "open".to_owned(),
            supplier_id: Uuid("00000000-0000-0000-0000-000000000003".to_owned()),
            purchase_order_row_version: 4,
            row_version: 4000,
            line_id: Uuid("00000000-0000-0000-0000-000000000004".to_owned()),
            line_number: 1,
            item_id: Uuid("00000000-0000-0000-0000-000000000005".to_owned()),
            item_number: "ITEM-1".to_owned(),
            ordered_quantity: Numeric("12.3400".to_owned()),
            received_quantity: Numeric("0".to_owned()),
            remaining_quantity: Numeric("12.3400".to_owned()),
        };
        let output = [screen_contract::LoadReceiptScreenOutcome {
            outcome: Ok(screen_contract::LoadReceiptScreenResult::Page(
                screen_contract::LoadReceiptScreenPage {
                    item: vec![screen::row!(row, screen_contract::LoadReceiptScreenRow)],
                    next_cursor: None,
                    cursor: "snapshot".to_owned(),
                },
            )),
        }];
        let value: serde_json::Value = serde_json::from_str(&screen::encode(&output)).unwrap();
        let row = &value[0]["value"]["item"][0];
        assert_eq!(row["purchase_order_row_version"], 4);
        assert_eq!(row["row_version"], "4000");
        assert_eq!(row["line_id"], "00000000-0000-0000-0000-000000000004");
        assert_eq!(row["ordered_quantity"], "12.3400");

        let row = wamn_receiving_data_access::read::PurchaseOrderSummaryRow {
            purchase_order_id: Uuid("00000000-0000-0000-0000-000000000002".to_owned()),
            purchase_order_number: "PO-1".to_owned(),
            status: "open".to_owned(),
            supplier_id: Uuid("00000000-0000-0000-0000-000000000003".to_owned()),
            supplier_name: "Supplier A".to_owned(),
            line_count: 2,
            ordered_quantity: Numeric("12.3400".to_owned()),
            received_quantity: Numeric("0".to_owned()),
        };
        let output = [summary_contract::PurchaseOrderSummaryOutcome {
            outcome: Ok(summary_contract::PurchaseOrderSummaryResult {
                rows: vec![summary::row!(
                    row,
                    summary_contract::PurchaseOrderSummaryRow
                )],
            }),
        }];
        let value: serde_json::Value = serde_json::from_str(&summary::encode(&output)).unwrap();
        let row = &value[0]["value"]["rows"][0];
        assert_eq!(row["line_count"], "2");
        assert_eq!(row["ordered_quantity"], "12.3400");
    }
}

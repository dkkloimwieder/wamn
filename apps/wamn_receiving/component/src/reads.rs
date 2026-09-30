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
        "field" if error.kind() == AccessErrorType::NotFound => {
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
    include!("../../generated/wit/location_list_codec.rs");

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
                map_error(error.kind().literal(), |key| {
                    error_detail(&error, key, "location.list", None, None)
                })
            })
    }
}

pub(super) mod receiving_load_receipt_screen {
    use super::{Connection, Uuid, error_detail, read};
    use crate::exports::wamn_receiving::receiving::load_receipt_screen as contract;
    include!("../../generated/wit/receiving_load_receipt_screen_codec.rs");

    pub(super) async fn execute(
        connection: &mut Connection,
        request: contract::LoadReceiptScreenRequest,
    ) -> Result<contract::LoadReceiptScreenResult, contract::LoadReceiptScreenError> {
        read::receipt_screen(connection, Uuid(request.purchase_order_id.clone()))
            .await
            .map(|rows| contract::LoadReceiptScreenResult {
                rows: rows
                    .into_iter()
                    .map(|value| row!(value, contract::LoadReceiptScreenRow))
                    .collect(),
            })
            .map_err(|error| {
                map_error(error.kind().literal(), |key| {
                    error_detail(
                        &error,
                        key,
                        "receiving.load_receipt_screen",
                        Some(("purchase_order_id", request.purchase_order_id.as_str())),
                        None,
                    )
                })
            })
    }
}

pub(super) mod receiving_load_purchase_order_history {
    use super::{Connection, Uuid, error_detail, read};
    use crate::exports::wamn_receiving::receiving::load_purchase_order_history as contract;
    include!("../../generated/wit/receiving_load_purchase_order_history_codec.rs");

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
            map_error(error.kind().literal(), |key| {
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

location_list::export_operation!(
    crate::Component,
    crate::exports::wamn_receiving::location::list,
    crate::wamn::node::types,
    Connection::new(),
    location_list::execute,
    location_list
);

receiving_load_receipt_screen::export_operation!(
    crate::Component,
    crate::exports::wamn_receiving::receiving::load_receipt_screen,
    crate::wamn::node::types,
    Connection::new(),
    receiving_load_receipt_screen::execute,
    receiving_load_receipt_screen
);

receiving_load_purchase_order_history::export_operation!(
    crate::Component,
    crate::exports::wamn_receiving::receiving::load_purchase_order_history,
    crate::wamn::node::types,
    Connection::new(),
    receiving_load_purchase_order_history::execute,
    receiving_load_purchase_order_history
);

#[cfg(test)]
mod tests {
    use super::{location_list as locations, receiving_load_receipt_screen as screen};
    use crate::exports::wamn_receiving::{
        location::list as location_contract, receiving::load_receipt_screen as screen_contract,
    };
    use crate::generated::purchase_order::get::codec as get;
    use wamn_postgres_statements::{Numeric, Uuid};

    #[test]
    fn a_read_item_carries_no_request_id() {
        let items = get::decode(
            r#"[
            {"request_id":"first","id":"00000000-0000-0000-0000-000000000001"},
            {"id":"00000000-0000-0000-0000-000000000002"}
        ]"#,
        )
        .unwrap();
        assert!(items[0].input.is_err(), "a read refuses a request identity");
        assert!(items[1].input.is_ok());
    }

    #[test]
    fn envelope_preserves_item_order() {
        let items = get::decode(
            r#"[
            {"id":"00000000-0000-0000-0000-000000000002"},
            {"id":"00000000-0000-0000-0000-000000000001"}
        ]"#,
        )
        .unwrap();
        assert_eq!(
            items[0].input.as_ref().unwrap().id,
            "00000000-0000-0000-0000-000000000002"
        );
        assert_eq!(
            items[1].input.as_ref().unwrap().id,
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

        let row = wamn_receiving_data_access::read::LoadReceiptScreenRow {
            purchase_order_id: Uuid("00000000-0000-0000-0000-000000000002".to_owned()),
            purchase_order_number: "PO-1".to_owned(),
            purchase_order_status: "open".to_owned(),
            supplier_id: Uuid("00000000-0000-0000-0000-000000000003".to_owned()),
            row_version: 4,
            line_id: None,
            line_number: None,
            item_id: None,
            item_number: None,
            ordered_quantity: Some(Numeric("12.3400".to_owned())),
            received_quantity: Some(Numeric("0".to_owned())),
            remaining_quantity: Some(Numeric("12.3400".to_owned())),
        };
        let output = [screen_contract::LoadReceiptScreenOutcome {
            outcome: Ok(screen_contract::LoadReceiptScreenResult {
                rows: vec![screen::row!(row, screen_contract::LoadReceiptScreenRow)],
            }),
        }];
        let value: serde_json::Value = serde_json::from_str(&screen::encode(&output)).unwrap();
        let row = &value[0]["value"]["rows"][0];
        assert_eq!(row["row_version"], 4, "a revision is an int32 number");
        assert!(row["line_id"].is_null());
        assert_eq!(row["ordered_quantity"], "12.3400");
    }
}

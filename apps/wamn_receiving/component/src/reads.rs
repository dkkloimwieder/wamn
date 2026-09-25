//! Typed read handlers over the existing application operations.

use wamn_postgres_statements::{Connection, Uuid};
use wamn_receiving_data_access::{AccessError, AccessErrorKind, purchase_order, read, receipt};

pub(super) fn error_detail(
    error: &AccessError,
    key: &str,
    operation: &str,
    id: Option<(&str, &str)>,
    expected: Option<i32>,
) -> Option<String> {
    match key {
        "field" if error.kind() == AccessErrorKind::NotFound => {
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

pub(super) mod purchase_order_get {
    use super::{Connection, error_detail, purchase_order};
    use crate::exports::wamn_receiving::purchase_order::get as contract;
    include!("../../generated/wit/purchase_order_get_codec.rs");

    pub(super) async fn execute(
        connection: &mut Connection,
        request: contract::GetRequest,
    ) -> Result<contract::GetResult, contract::GetError> {
        purchase_order::get(connection, &request.id)
            .await
            .map(|value| contract::GetResult {
                value: row!(value, contract::GetRow),
            })
            .map_err(|error| {
                map_error(error.kind().literal(), |key| {
                    error_detail(
                        &error,
                        key,
                        "purchase_order.get",
                        Some(("id", &request.id)),
                        None,
                    )
                })
            })
    }
}

pub(super) mod receipt_get {
    use super::{Connection, error_detail, receipt};
    use crate::exports::wamn_receiving::receipt::get as contract;
    include!("../../generated/wit/receipt_get_codec.rs");

    pub(super) async fn execute(
        connection: &mut Connection,
        request: contract::GetRequest,
    ) -> Result<contract::GetResult, contract::GetError> {
        receipt::get(connection, &request.id)
            .await
            .map(|value| contract::GetResult {
                value: row!(value, contract::GetRow),
            })
            .map_err(|error| {
                map_error(error.kind().literal(), |key| {
                    error_detail(&error, key, "receipt.get", Some(("id", &request.id)), None)
                })
            })
    }
}

pub(super) mod receipt_query {
    use super::{Connection, error_detail, receipt};
    use crate::exports::wamn_receiving::receipt::query as contract;
    include!("../../generated/wit/receipt_query_codec.rs");

    pub(super) async fn execute(
        connection: &mut Connection,
        request: contract::QueryRequest,
    ) -> Result<contract::QueryResult, contract::QueryError> {
        let input = receipt::QueryInput {
            cursor: request.cursor.map(Into::into),
            limit: request.limit,
        };
        receipt::query(connection, &input)
            .await
            .map(|page| contract::QueryResult {
                value: page
                    .item
                    .into_vec()
                    .into_iter()
                    .map(|value| row!(value, contract::QueryRow))
                    .collect(),
                next_cursor: page.next_cursor.map(Into::into),
            })
            .map_err(|error| {
                map_error(error.kind().literal(), |key| {
                    error_detail(&error, key, "receipt.query", None, None)
                })
            })
    }
}

pub(super) mod purchase_order_query {
    use super::{Connection, error_detail, purchase_order};
    use crate::exports::wamn_receiving::purchase_order::query as contract;
    use purchase_order::PurchaseOrderSort as Sort;
    include!("../../generated/wit/purchase_order_query_codec.rs");

    pub(super) async fn execute(
        connection: &mut Connection,
        request: contract::QueryRequest,
    ) -> Result<contract::QueryResult, contract::QueryError> {
        let statuses = request
            .status
            .map(|values| {
                values
                    .into_iter()
                    .map(|value| match value.as_str() {
                        "open" => Ok(purchase_order::PurchaseOrderStatus::Open),
                        "complete" => Ok(purchase_order::PurchaseOrderStatus::Complete),
                        "cancelled" => Ok(purchase_order::PurchaseOrderStatus::Cancelled),
                        _ => Err(map_error("invalid_input", |key| {
                            (key == "field").then(|| "input".to_owned())
                        })),
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(Vec::into_boxed_slice)
            })
            .transpose()?;
        let sort = match (
            request.sort_field.as_deref(),
            request.sort_direction.as_deref(),
        ) {
            (None, None) => Sort::default(),
            (Some("purchase_order_number"), Some("ascending")) => {
                Sort::PurchaseOrderNumberAscending
            }
            (Some("purchase_order_number"), Some("descending")) => {
                Sort::PurchaseOrderNumberDescending
            }
            (Some("status"), Some("ascending")) => Sort::StatusAscending,
            (Some("status"), Some("descending")) => Sort::StatusDescending,
            (Some("created_at"), Some("ascending")) => Sort::CreatedAtAscending,
            (Some("created_at"), Some("descending")) => Sort::CreatedAtDescending,
            _ => {
                return Err(map_error("invalid_input", |key| {
                    (key == "field").then(|| "input".to_owned())
                }));
            }
        };
        let input = purchase_order::QueryInput {
            supplier_ids: request.supplier_id.map(|values| {
                values
                    .into_iter()
                    .map(Into::into)
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            }),
            statuses,
            purchase_order_numbers: request.purchase_order_number.map(|values| {
                values
                    .into_iter()
                    .map(Into::into)
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            }),
            sort,
            cursor: request.cursor.map(Into::into),
            limit: request.limit,
        };
        purchase_order::query(connection, &input)
            .await
            .map(|page| contract::QueryResult {
                value: page
                    .item
                    .into_vec()
                    .into_iter()
                    .map(|value| row!(value, contract::QueryRow))
                    .collect(),
                next_cursor: page.next_cursor.map(Into::into),
            })
            .map_err(|error| {
                map_error(error.kind().literal(), |key| {
                    error_detail(&error, key, "purchase_order.query", None, None)
                })
            })
    }
}

pub(super) mod supplier_query {
    use super::{Connection, error_detail};
    use crate::exports::wamn_receiving::supplier::query as contract;
    use wamn_receiving_data_access::supplier;
    include!("../../generated/wit/supplier_query_codec.rs");

    pub(super) async fn execute(
        connection: &mut Connection,
        request: contract::QueryRequest,
    ) -> Result<contract::QueryResult, contract::QueryError> {
        let input = supplier::QueryInput {
            cursor: request.cursor.map(Into::into),
            limit: request.limit,
        };
        supplier::query(connection, &input)
            .await
            .map(|page| contract::QueryResult {
                value: page
                    .item
                    .into_vec()
                    .into_iter()
                    .map(|value| row!(value, contract::QueryRow))
                    .collect(),
                next_cursor: page.next_cursor.map(Into::into),
            })
            .map_err(|error| {
                map_error(error.kind().literal(), |key| {
                    error_detail(&error, key, "supplier.query", None, None)
                })
            })
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

purchase_order_get::export_operation!(
    crate::Component,
    crate::exports::wamn_receiving::purchase_order::get,
    crate::wamn::node::types,
    Connection::new(),
    purchase_order_get::execute,
    purchase_order_get
);

purchase_order_query::export_operation!(
    crate::Component,
    crate::exports::wamn_receiving::purchase_order::query,
    crate::wamn::node::types,
    Connection::new(),
    purchase_order_query::execute,
    purchase_order_query
);

receipt_get::export_operation!(
    crate::Component,
    crate::exports::wamn_receiving::receipt::get,
    crate::wamn::node::types,
    Connection::new(),
    receipt_get::execute,
    receipt_get
);

receipt_query::export_operation!(
    crate::Component,
    crate::exports::wamn_receiving::receipt::query,
    crate::wamn::node::types,
    Connection::new(),
    receipt_query::execute,
    receipt_query
);

supplier_query::export_operation!(
    crate::Component,
    crate::exports::wamn_receiving::supplier::query,
    crate::wamn::node::types,
    Connection::new(),
    supplier_query::execute,
    supplier_query
);

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
    use super::{
        location_list as locations, purchase_order_get as get,
        receiving_load_receipt_screen as screen,
    };
    use crate::exports::wamn_receiving::{
        location::list as location_contract, receiving::load_receipt_screen as screen_contract,
    };
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
            super::purchase_order_query::decode(r#"[{"limit":2}]"#).unwrap()[0]
                .input
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

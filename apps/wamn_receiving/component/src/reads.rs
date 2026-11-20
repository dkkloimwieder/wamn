//! Typed read handlers over the existing application operations.

use crate::data::{AccessError, AccessErrorType, read};
use wamn_guest::postgres_statements::{Connection, Uuid};

impl std::str::FromStr
    for crate::exports::wamn_receiving::receiving::load_open_receipt_lines::PurchaseOrderStatusValue
{
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "open" => Ok(Self::Open),
            "complete" => Ok(Self::Complete),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(()),
        }
    }
}

impl std::str::FromStr
    for crate::exports::wamn_receiving::receiving::load_receipt_screen::PurchaseOrderStatusValue
{
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "open" => Ok(Self::Open),
            "complete" => Ok(Self::Complete),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(()),
        }
    }
}

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
    include!("../../../target/wamn/wamn_receiving/wit/location_list_operation.rs");

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
        "../../../target/wamn/wamn_receiving/wit/receiving_load_purchase_order_history_operation.rs"
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
        .and_then(|rows| {
            rows.into_iter()
                .map(|value| {
                    let type_ = match value.type_.as_str() {
                        "insert" => contract::TypeValue::Insert,
                        "update" => contract::TypeValue::Update,
                        "delete" => contract::TypeValue::Delete,
                        _ => {
                            return Err(crate::data::AccessError::internal("unknown history event"));
                        }
                    };
                    Ok(contract::LoadPurchaseOrderHistoryRow {
                        id: value.id.0,
                        cursor: value.cursor,
                        type_,
                        operation: value.operation,
                        changed_by: value.changed_by.0,
                        changed_at: value.changed_at.0,
                        before: value.before,
                        after: value.after,
                        current: value.current,
                    })
                })
                .collect::<Result<_, _>>()
                .map(|rows| contract::LoadPurchaseOrderHistoryResult { rows })
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
    include!(
        "../../../target/wamn/wamn_receiving/wit/receiving_purchase_order_summary_operation.rs"
    );

    pub(super) async fn execute(
        connection: &mut Connection,
        request: contract::PurchaseOrderSummaryRequest,
    ) -> Result<contract::PurchaseOrderSummaryResult, contract::PurchaseOrderSummaryError> {
        let status = match request.status {
            contract::StatusValue::Open => "open",
            contract::StatusValue::Complete => "complete",
            contract::StatusValue::Cancelled => "cancelled",
        };
        read::purchase_order_summary(connection, Uuid(request.supplier_id), status.to_owned())
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

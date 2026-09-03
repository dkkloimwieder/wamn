#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every Receiving operation.

use exports::wamn_receiving::location::list::Guest as LocationList;
use exports::wamn_receiving::purchase_order::get::Guest as PurchaseOrderGet;
use exports::wamn_receiving::purchase_order::query::Guest as PurchaseOrderQuery;
use exports::wamn_receiving::purchase_order::update::Guest as PurchaseOrderUpdate;
use exports::wamn_receiving::receipt::get::Guest as ReceiptGet;
use exports::wamn_receiving::receipt::query::Guest as ReceiptQuery;
use exports::wamn_receiving::receiving::load_receipt_screen::Guest as LoadReceiptScreen;
use exports::wamn_receiving::receiving::record_receipt::Guest as RecordReceipt;
use wamn::node::types::{Emission, NodeContext, NodeError};

wit_bindgen::generate!({
    world: "receiving",
    path: "../../data/receiving-data/wit",
    generate_all,
});

struct Component;

fn invoke_operation<F>(operation: F) -> Result<Emission, NodeError>
where
    F: Future<Output = Result<String, wamn_receiving_data_access::operation::InvocationError>>,
{
    futures_executor::block_on(operation)
        .map(|payload| Emission {
            payload,
            port: None,
        })
        .map_err(|error| {
            NodeError::InvalidInput(wamn::node::types::ErrorDetail {
                message: error.context().to_owned(),
                code: Some(error.code().to_owned()),
            })
        })
}

impl LocationList for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(wamn_receiving_data_access::operation::location_list(&input))
    }
}

impl PurchaseOrderGet for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(wamn_receiving_data_access::operation::purchase_order_get(
            &input,
        ))
    }
}

impl PurchaseOrderQuery for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(wamn_receiving_data_access::operation::purchase_order_query(
            &input,
        ))
    }
}

impl PurchaseOrderUpdate for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(wamn_receiving_data_access::operation::purchase_order_update(&input))
    }
}

impl ReceiptGet for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(wamn_receiving_data_access::operation::receipt_get(&input))
    }
}

impl ReceiptQuery for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(wamn_receiving_data_access::operation::receipt_query(&input))
    }
}

impl LoadReceiptScreen for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(wamn_receiving_data_access::operation::receiving_load_receipt_screen(
            &input,
        ))
    }
}

impl RecordReceipt for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(wamn_receiving_data_access::operation::receiving_record_receipt(&input))
    }
}

export!(Component);

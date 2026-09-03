#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting the WMS operations.
//!
//! `inventory.move` alone for now: the composed-wiring gate asks for one
//! wiring and one contention proof, and the remaining six operations are
//! follow-on work rather than a prerequisite for it.

use exports::wamn_wms::inventory::move_::Guest as InventoryMove;
use wamn::node::types::{Emission, NodeContext, NodeError};

wit_bindgen::generate!({
    world: "wms",
    path: "../../data/wms-data/wit",
    generate_all,
});

struct Component;

fn invoke_operation<F>(operation: F) -> Result<Emission, NodeError>
where
    F: Future<Output = Result<String, wamn_wms_data_access::operation::InvocationError>>,
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

impl InventoryMove for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(wamn_wms_data_access::operation::inventory_move_operation(
            &input,
        ))
    }
}

export!(Component);

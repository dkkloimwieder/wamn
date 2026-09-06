#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every WMS operation.

use exports::wamn_wms::inventory::adjust::Guest as InventoryAdjust;
use exports::wamn_wms::inventory::aggregate::Guest as InventoryAggregate;
use exports::wamn_wms::inventory::merge::Guest as InventoryMerge;
use exports::wamn_wms::inventory::move_::Guest as InventoryMove;
use exports::wamn_wms::inventory::split::Guest as InventorySplit;
use exports::wamn_wms::pallet::get::Guest as PalletGet;
use exports::wamn_wms::pallet::query::Guest as PalletQuery;
use wamn::node::types::{Emission, NodeContext, NodeError};
use wamn_wms_data_access::operation;

wit_bindgen::generate!({
    world: "wms",
    path: "../../data/wms-data/wit",
    generate_all,
});

struct Component;

fn invoke_operation<F>(operation: F) -> Result<Emission, NodeError>
where
    F: Future<Output = Result<String, operation::InvocationError>>,
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

impl InventoryAdjust for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(operation::inventory_adjust_operation(&input))
    }
}

impl InventoryAggregate for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(operation::inventory_aggregate_operation(&input))
    }
}

impl InventoryMerge for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(operation::inventory_merge_operation(&input))
    }
}

impl InventoryMove for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(operation::inventory_move_operation(&input))
    }
}

impl InventorySplit for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(operation::inventory_split_operation(&input))
    }
}

impl PalletGet for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(operation::pallet_get_operation(&input))
    }
}

impl PalletQuery for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_operation(operation::pallet_query_operation(&input))
    }
}

export!(Component);

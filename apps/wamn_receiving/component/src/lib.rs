#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component exporting every Receiving operation.
#![expect(unsafe_code, reason = "wit-bindgen generated FFI glue")]
#[expect(
    dead_code,
    reason = "private data modules retain the complete generated and authored refusal vocabulary"
)]
mod data;

use crate::data::record_receipt as receipt;
use exports::wamn_receiving::receiving::record_receipt as contract;

mod reads;

/// The generated operations, whole.
mod generated {
    use crate::data::generated as data;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/wamn_receiving/component/mod.rs"
    ));
}

wit_bindgen::generate!({
    world: "wamn:receiving-component/receiving@0.1.0",
    path: [
        "../../../wit/wamn-node",
        "../../../wit/wamn-postgres-0.4",
        "../../target/wamn/wamn_receiving/wit",
        "wit",
    ],
    generate_all,
    async: true,
});

struct Component;
export!(Component);

impl generated::purchase_order::create::Action for Component {}
impl generated::purchase_order::get::Action for Component {}
impl generated::purchase_order::query::Action for Component {}
impl generated::purchase_order::update::Action for Component {}
impl generated::receipt::get::Action for Component {}
impl generated::receipt::query::Action for Component {}
impl generated::supplier::create::Action for Component {}
impl generated::supplier::query::Action for Component {}

mod receipt_operation {
    use super::contract;

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/wamn_receiving/wit/receiving_record_receipt_operation.rs"
    ));
}

receipt_operation::export_operation!(
    Component,
    exports::wamn_receiving::receiving::record_receipt,
    wamn::node::types,
    record_receipt_execute,
    receipt_operation
);

fn record_receipt_command(request: contract::RecordReceiptRequest) -> receipt::RecordReceiptValue {
    receipt::RecordReceiptValue {
        idempotency_key: request.idempotency_key.into_boxed_str(),
        purchase_order_id: request.purchase_order_id.into_boxed_str(),
        receipt_reference: request.receipt_reference.into_boxed_str(),
        occurred_at: request.occurred_at.into_boxed_str(),
        line: request
            .line
            .into_iter()
            .map(|line| receipt::RecordReceiptLine {
                purchase_order_line_id: line.purchase_order_line_id.into_boxed_str(),
                quantity: line.quantity.into_boxed_str(),
                location_id: line.location_id.into_boxed_str(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    }
}

fn map_record_receipt_error(error: &receipt::RecordReceiptError) -> contract::RecordReceiptError {
    receipt_operation::map_error(error.error_type().literal(), |key| match key {
        "field" => error.field().map(str::to_owned),
        "id" => error.id().map(str::to_owned),
        "minimum" => error.minimum().map(|value| value.to_string()),
        "maximum" => error.maximum().map(|value| value.to_string()),
        "observed" => error.observed().map(|value| value.to_string()),
        "constraint" => error.constraint().map(str::to_owned),
        "operation" => Some("receiving.record_receipt".to_owned()),
        _ => None,
    })
}

async fn record_receipt_execute(
    transaction: &mut wamn_postgres_statements::Transaction,
    request: contract::RecordReceiptRequest,
) -> Result<contract::RecordReceiptResult, contract::RecordReceiptError> {
    receipt::execute(transaction, &record_receipt_command(request))
        .await
        .map(|value| contract::RecordReceiptResult {
            receipt_id: value.receipt_id.into(),
            purchase_order_id: value.purchase_order_id.into(),
            purchase_order_status: value.purchase_order_status.as_str().to_owned(),
            row_version: value.row_version,
        })
        .map_err(|error| map_record_receipt_error(&error))
}

#[cfg(test)]
mod validation;

//! Typed Acme quality-operation boundaries.

use super::exports::client_acme_receiving::quality::{
    approve_inspection as approve_contract, create_inspection as create_contract,
    load_purchase_order_detail as detail_contract,
};
use super::{AccessError, NodeError, access_detail, private_node_error};
use wamn_client_acme_receiving_data_access::operation;

pub(super) mod detail_codec {
    use super::detail_contract as contract;

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wit/quality_load_purchase_order_detail_codec.rs"
    ));
}

pub(super) mod approve_codec {
    use super::approve_contract as contract;

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wit/quality_approve_inspection_codec.rs"
    ));
}

pub(super) async fn handle_detail(
    connection: &mut wamn_postgres_statements::Connection,
    request: detail_contract::LoadPurchaseOrderDetailRequest,
) -> Result<
    detail_contract::LoadPurchaseOrderDetailResult,
    detail_contract::LoadPurchaseOrderDetailError,
> {
    operation::quality_load_purchase_order_detail(connection, &request.purchase_order_id)
        .await
        .map(|row| detail_codec::row!(row, detail_contract::LoadPurchaseOrderDetailResult))
        .map_err(|error| detail_error(&error, &request.purchase_order_id))
}

pub(super) async fn handle_approve(
    connection: &mut wamn_postgres_statements::Connection,
    request: approve_contract::ApproveInspectionRequest,
) -> Result<approve_contract::ApproveInspectionResult, approve_contract::ApproveInspectionError> {
    operation::quality_approve_inspection(
        connection,
        &request.receipt_id,
        request.expected_row_version,
    )
    .await
    .map(|row| approve_codec::row!(row, approve_contract::ApproveInspectionResult))
    .map_err(|error| approve_error(&error, &request.receipt_id, request.expected_row_version))
}

pub(super) async fn handle_create(
    _state: &mut (),
    input: create_contract::CreateInspectionRequest,
) -> Result<create_contract::CreateInspectionRequest, NodeError> {
    operation::quality_create_inspection(&input.event, &input.new.id)
        .await
        .map_err(|error| private_node_error(&error))?;
    Ok(input)
}

fn detail_error(error: &AccessError, id: &str) -> detail_contract::LoadPurchaseOrderDetailError {
    detail_codec::map_error(error.kind().literal(), |key| {
        access_detail(
            error,
            key,
            "quality.load_purchase_order_detail",
            Some(("purchase_order_id", id)),
            None,
        )
    })
}

fn approve_error(
    error: &AccessError,
    id: &str,
    expected: i32,
) -> approve_contract::ApproveInspectionError {
    approve_codec::map_error(error.kind().literal(), |key| {
        access_detail(
            error,
            key,
            "quality.approve_inspection",
            Some(("receipt_id", id)),
            Some(expected),
        )
    })
}

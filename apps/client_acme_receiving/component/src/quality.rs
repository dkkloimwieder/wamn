//! Typed Acme quality-operation boundaries.

use super::exports::client_acme_receiving::quality::{
    approve_inspection as approve_contract, create_inspection as create_contract,
    load_purchase_order_detail as detail_contract,
};
use super::{AccessError, NodeError, access_detail, private_node_error};
use crate::data::operation;

pub(super) mod detail_operation {
    use super::detail_contract as contract;

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/client_acme_receiving/wit/quality_load_purchase_order_detail_operation.rs"
    ));
}

pub(super) mod approve_operation {
    use super::approve_contract as contract;

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/client_acme_receiving/wit/quality_approve_inspection_operation.rs"
    ));
}

pub(super) async fn handle_detail(
    connection: &mut wamn_guest::postgres_statements::Connection,
    request: detail_contract::LoadPurchaseOrderDetailRequest,
) -> Result<
    detail_contract::LoadPurchaseOrderDetailResult,
    detail_contract::LoadPurchaseOrderDetailError,
> {
    operation::quality_load_purchase_order_detail(connection, &request.purchase_order_id)
        .await
        .map_err(|error| detail_error(&error, &request.purchase_order_id))
        .and_then(|row| {
            let status = match row.status.as_str() {
                "open" => detail_contract::StatusValue::Open,
                "complete" => detail_contract::StatusValue::Complete,
                "cancelled" => detail_contract::StatusValue::Cancelled,
                _ => return Err(detail_contract::LoadPurchaseOrderDetailError::InternalError),
            };
            let acme_quality_status = match row.acme_quality_status.as_str() {
                "not_required" => detail_contract::AcmeQualityStatusValue::NotRequired,
                "pending" => detail_contract::AcmeQualityStatusValue::Pending,
                "approved" => detail_contract::AcmeQualityStatusValue::Approved,
                _ => return Err(detail_contract::LoadPurchaseOrderDetailError::InternalError),
            };
            Ok(detail_contract::LoadPurchaseOrderDetailResult {
                id: row.id.0,
                purchase_order_number: row.purchase_order_number,
                supplier_id: row.supplier_id.0,
                status,
                row_version: row.row_version,
                acme_inspection_required: row.acme_inspection_required,
                acme_quality_status,
            })
        })
}

pub(super) async fn handle_approve(
    transaction: &mut wamn_guest::postgres_statements::Transaction,
    request: approve_contract::ApproveInspectionRequest,
) -> Result<approve_contract::ApproveInspectionResult, approve_contract::ApproveInspectionError> {
    operation::quality_approve_inspection(
        transaction,
        &request.receipt_id,
        request.expected_row_version,
    )
    .await
    .map(|row| approve_contract::ApproveInspectionResult {
        receipt_id: row.receipt_id.0,
        status: approve_contract::StatusValue::Approved,
        row_version: row.row_version,
        purchase_order_id: row.purchase_order_id.0,
        purchase_order_row_version: row.purchase_order_row_version,
    })
    .map_err(|error| approve_error(&error, &request.receipt_id, request.expected_row_version))
}

pub(super) async fn handle_create(
    _state: &mut (),
    input: create_contract::CreateInspectionRequest,
) -> Result<create_contract::CreateInspectionRequest, NodeError> {
    let create_contract::EventValue::Insert = input.event;
    operation::quality_create_inspection("insert", &input.new.id)
        .await
        .map_err(|error| private_node_error(&error))?;
    Ok(input)
}

fn detail_error(error: &AccessError, id: &str) -> detail_contract::LoadPurchaseOrderDetailError {
    detail_operation::map_error(error.error_type().literal(), |key| {
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
    approve_operation::map_error(error.error_type().literal(), |key| {
        access_detail(
            error,
            key,
            "quality.approve_inspection",
            Some(("receipt_id", id)),
            Some(expected),
        )
    })
}

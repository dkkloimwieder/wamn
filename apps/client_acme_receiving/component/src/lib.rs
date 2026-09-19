#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component for executable Acme Receiving overlay operations.

use exports::client_acme_receiving::purchase_order::get::Guest as PurchaseOrderGet;
use exports::client_acme_receiving::purchase_order::update::Guest as PurchaseOrderUpdate;
use exports::client_acme_receiving::quality::approve_inspection::Guest as ApproveInspection;
use exports::client_acme_receiving::quality::create_inspection::Guest as CreateInspection;
use exports::client_acme_receiving::quality::load_purchase_order_detail::Guest as LoadPurchaseOrderDetail;
use exports::client_acme_receiving::receiving::record_receipt::Guest as RecordReceipt;
use wamn::node::types::{Emission, ErrorDetail, NodeContext, NodeError};
use wamn_client_acme_receiving_data_access::operation::InvocationError;
use wamn_client_acme_receiving_data_access::{AccessError, AccessErrorKind};

mod update;

wit_bindgen::generate!({
    world: "client-acme-receiving:component/client-acme-receiving@3.0.0",
    inline: r#"
        package client-acme-receiving:component@3.0.0;

        world client-acme-receiving {
          import wamn:postgres/types@0.1.0;
          import wamn:postgres/statements@0.1.0;
          import wamn-receiving:receiving/record-receipt@1.0.0;
          export client-acme-receiving:purchase-order/get@3.0.0;
          export client-acme-receiving:purchase-order/update@3.0.0;
          export client-acme-receiving:quality/load-purchase-order-detail@3.0.0;
          export client-acme-receiving:quality/approve-inspection@3.0.0;
          export client-acme-receiving:quality/create-inspection@3.0.0;
          export client-acme-receiving:receiving/record-receipt@3.0.0;
        }
    "#,
    path: [
        "../../wamn_receiving/data/wit/deps/wamn-node",
        "../../wamn_receiving/data/wit/deps/wamn-postgres",
        "../generated/wit/deps/client-acme-receiving-purchase-order",
        "wit/deps/client-acme-receiving-quality",
        "../../wamn_receiving/generated/wit/deps/wamn-receiving-receiving",
        "../generated/wit/deps/client-acme-receiving-receiving",
    ],
    generate_all,
    async: true,
});

struct Component;

fn emission(payload: String) -> Emission {
    Emission {
        payload,
        port: None,
    }
}

async fn invoke_public<F>(operation: F) -> Result<Emission, NodeError>
where
    F: Future<Output = Result<String, InvocationError>>,
{
    operation.await.map(emission).map_err(|error| {
        NodeError::InvalidInput(ErrorDetail {
            message: error.context().to_owned(),
            code: Some(error.code().to_owned()),
        })
    })
}

async fn invoke_private<F>(operation: F) -> Result<Emission, NodeError>
where
    F: Future<Output = Result<String, AccessError>>,
{
    operation
        .await
        .map(emission)
        .map_err(|error| private_node_error(&error))
}

fn private_node_error(error: &AccessError) -> NodeError {
    let kind = error.kind();
    let code = match kind {
        AccessErrorKind::InvalidInput => "invalid_input",
        AccessErrorKind::Retry => "retry",
        AccessErrorKind::Timeout => "timeout",
        AccessErrorKind::NotFound
        | AccessErrorKind::ConcurrencyConflict
        | AccessErrorKind::ExclusionViolation
        | AccessErrorKind::PermissionDenied
        | AccessErrorKind::InternalError => "internal_error",
    };
    let detail = ErrorDetail {
        message: error.context().to_owned(),
        code: Some(code.to_owned()),
    };
    match kind {
        AccessErrorKind::InvalidInput => NodeError::InvalidInput(detail),
        AccessErrorKind::Retry | AccessErrorKind::Timeout => NodeError::Retryable(detail),
        AccessErrorKind::NotFound
        | AccessErrorKind::ConcurrencyConflict
        | AccessErrorKind::ExclusionViolation
        | AccessErrorKind::PermissionDenied
        | AccessErrorKind::InternalError => NodeError::Terminal(detail),
    }
}

impl PurchaseOrderGet for Component {
    async fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_public(wamn_client_acme_receiving_data_access::operation::purchase_order_get(&input))
            .await
    }
}

impl PurchaseOrderUpdate for Component {
    async fn run(
        _context: NodeContext,
        input: Vec<exports::client_acme_receiving::purchase_order::update::UpdateItem>,
    ) -> Result<Vec<exports::client_acme_receiving::purchase_order::update::UpdateOutcome>, NodeError>
    {
        update::run(input).await
    }

    async fn run_json(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let input = update::codec::decode(&input).map_err(|error| {
            NodeError::InvalidInput(ErrorDetail {
                message: error.context().to_owned(),
                code: Some("invalid_input".to_owned()),
            })
        })?;
        let output = update::run(input).await?;
        Ok(emission(update::codec::encode(&output)))
    }
}

impl LoadPurchaseOrderDetail for Component {
    async fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_public(
            wamn_client_acme_receiving_data_access::operation::quality_load_purchase_order_detail(
                &input,
            ),
        )
        .await
    }
}

impl ApproveInspection for Component {
    async fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_public(
            wamn_client_acme_receiving_data_access::operation::quality_approve_inspection(&input),
        )
        .await
    }
}

impl CreateInspection for Component {
    async fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_private(
            wamn_client_acme_receiving_data_access::operation::quality_create_inspection(&input),
        )
        .await
    }
}

use wamn_receiving::receiving::record_receipt as contract;

mod receipt_codec {
    use super::contract;
    include!("../../generated/wit/receiving_record_receipt_codec.rs");
}

impl RecordReceipt for Component {
    async fn run(
        context: NodeContext,
        input: Vec<contract::RecordReceiptItem>,
    ) -> Result<Vec<contract::RecordReceiptOutcome>, NodeError> {
        contract::run(context, input).await
    }

    async fn run_json(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let input = receipt_codec::decode(&input).map_err(|error| {
            NodeError::InvalidInput(ErrorDetail {
                message: error.context().to_owned(),
                code: Some("invalid_input".to_owned()),
            })
        })?;
        let output = <Self as RecordReceipt>::run(context, input).await?;
        Ok(emission(receipt_codec::encode(&output)))
    }
}

export!(Component);

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
        "../../data/receiving-data/wit/deps/wamn-node",
        "../../data/receiving-data/wit/deps/wamn-postgres",
        "wit/deps/client-acme-receiving-purchase-order",
        "wit/deps/client-acme-receiving-quality",
        "wit/deps/client-acme-receiving-receiving",
        "wit/deps/wamn-receiving-receiving",
    ],
    generate_all,
});

struct Component;

fn emission(payload: String) -> Emission {
    Emission {
        payload,
        port: None,
    }
}

fn invoke_public<F>(operation: F) -> Result<Emission, NodeError>
where
    F: Future<Output = Result<String, InvocationError>>,
{
    futures_executor::block_on(operation)
        .map(emission)
        .map_err(|error| {
            NodeError::InvalidInput(ErrorDetail {
                message: error.context().to_owned(),
                code: Some(error.code().to_owned()),
            })
        })
}

fn invoke_private<F>(operation: F) -> Result<Emission, NodeError>
where
    F: Future<Output = Result<String, AccessError>>,
{
    futures_executor::block_on(operation)
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
        | AccessErrorKind::PermissionDenied
        | AccessErrorKind::InternalError => NodeError::Terminal(detail),
    }
}

impl PurchaseOrderGet for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_public(wamn_client_acme_receiving_data_access::operation::purchase_order_get(&input))
    }
}

impl PurchaseOrderUpdate for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_public(
            wamn_client_acme_receiving_data_access::operation::purchase_order_update(&input),
        )
    }
}

impl LoadPurchaseOrderDetail for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_public(
            wamn_client_acme_receiving_data_access::operation::quality_load_purchase_order_detail(
                &input,
            ),
        )
    }
}

impl ApproveInspection for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_public(
            wamn_client_acme_receiving_data_access::operation::quality_approve_inspection(&input),
        )
    }
}

impl CreateInspection for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        invoke_private(
            wamn_client_acme_receiving_data_access::operation::quality_create_inspection(&input),
        )
    }
}

impl RecordReceipt for Component {
    fn run(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let mut base = wamn_receiving::receiving::record_receipt::run(&context, &input)?;
        base.payload = futures_executor::block_on(
            wamn_client_acme_receiving_data_access::operation::receiving_record_receipt_result(
                &base.payload,
            ),
        )
        .map_err(|error| {
            NodeError::Terminal(ErrorDetail {
                message: error.context().to_owned(),
                code: Some("internal_error".to_owned()),
            })
        })?;
        Ok(base)
    }
}

export!(Component);

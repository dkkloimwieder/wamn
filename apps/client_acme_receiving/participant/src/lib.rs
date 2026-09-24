#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! The Acme Receiving record-receipt participant, as its own component.
//!
//! The base Receiving component calls it through its pre-commit import. It
//! exports the base's pre-commit interface and types its own interface with the
//! base's request record, so composition plugs it into that import. It imports
//! no base interface, so the base, the overlay and this participant compose
//! without a cycle.

use exports::client_acme_receiving::receiving::record_receipt_participant::Guest as RecordReceiptParticipant;
use exports::wamn_receiving::receiving::record_receipt_pre_commit::{
    Guest as RecordReceiptPreCommit, RecordReceiptPreCommitRequest,
};
use wamn::node::types::{Emission, ErrorDetail, NodeContext, NodeError};
use wamn_client_acme_receiving_data_access::{AccessError, AccessErrorKind};

wit_bindgen::generate!({
    world: "client-acme-receiving:participant/client-acme-receiving-participant@3.0.0",
    inline: r#"
        package client-acme-receiving:participant@3.0.0;

        world client-acme-receiving-participant {
          import wamn:postgres/types@0.1.0;
          import wamn:postgres/statements@0.1.0;
          export wamn-receiving:receiving/record-receipt-pre-commit@1.0.0;
          export client-acme-receiving:receiving/record-receipt-participant@3.0.0;
        }
    "#,
    path: [
        "../../../crates/execution/workflow/router/wit",
        "../../../crates/platform/runtime/wit/deps/wamn-postgres",
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

fn participant_node_error(error: &AccessError) -> NodeError {
    match error.kind() {
        AccessErrorKind::Timeout => NodeError::Cancelled,
        AccessErrorKind::PermissionDenied => NodeError::Terminal(ErrorDetail {
            message: error.context().to_owned(),
            code: Some("permission_denied".to_owned()),
        }),
        _ => private_node_error(error),
    }
}

mod receipt_participant_codec {
    use super::exports::client_acme_receiving::receiving::record_receipt_participant as contract;
    include!("../../generated/wit/receiving_record_receipt_participant_codec.rs");
}

impl RecordReceiptParticipant for Component {
    async fn run(
        _context: NodeContext,
        mut input: RecordReceiptPreCommitRequest,
    ) -> Result<RecordReceiptPreCommitRequest, NodeError> {
        receipt_participant_codec::normalize(&mut input).map_err(|error| {
            NodeError::InvalidInput(ErrorDetail {
                message: error.context().to_owned(),
                code: Some("invalid_input".to_owned()),
            })
        })?;
        let mut transaction =
            wamn_postgres_statements::participant_view()
                .await
                .map_err(|error| {
                    participant_node_error(
                        &wamn_client_acme_receiving_data_access::operation::participant_view_error(
                            &error,
                        ),
                    )
                })?;
        wamn_client_acme_receiving_data_access::operation::record_receipt_participant(
            &mut transaction,
            &input.receipt_id,
            &input.purchase_order_id,
        )
        .await
        .map_err(|error| participant_node_error(&error))?;
        Ok(input)
    }

    async fn run_json(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let request = receipt_participant_codec::decode(&input).map_err(|error| {
            NodeError::InvalidInput(ErrorDetail {
                message: error.context().to_owned(),
                code: Some("invalid_input".to_owned()),
            })
        })?;
        <Self as RecordReceiptParticipant>::run(context, request).await?;
        Ok(emission(input))
    }
}

impl RecordReceiptPreCommit for Component {
    async fn run(
        context: NodeContext,
        input: RecordReceiptPreCommitRequest,
    ) -> Result<RecordReceiptPreCommitRequest, NodeError> {
        <Self as RecordReceiptParticipant>::run(context, input).await
    }
}

export!(Component);

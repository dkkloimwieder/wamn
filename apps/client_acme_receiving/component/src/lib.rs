#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component for executable Acme Receiving overlay operations.

use exports::client_acme_receiving::receiving::record_receipt::Guest as RecordReceipt;
use wamn::node::types::{Emission, ErrorDetail, NodeContext, NodeError};
use wamn_client_acme_receiving_data_access::{AccessError, AccessErrorType};

#[cfg(test)]
mod codecs;
mod quality;

use quality::{approve_codec, detail_codec};

/// The generated operations, whole.
mod generated {
    use wamn_client_acme_receiving_data_access::generated as data;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/component/mod.rs"
    ));
}

mod create_codec {
    use super::exports::client_acme_receiving::quality::create_inspection as contract;

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wit/quality_create_inspection_codec.rs"
    ));
}

wit_bindgen::generate!({
    world: "client-acme-receiving:component/client-acme-receiving@4.0.0",
    path: [
        "../../../crates/execution/workflow/router/wit",
        "../../../crates/platform/runtime/wit/deps/wamn-postgres-0.3",
        "../../wamn_receiving/generated/wit/deps/wamn-receiving-receiving",
        "../generated/wit",
        "wit",
    ],
    generate_all,
    async: true,
});

struct Component;

quality::detail_codec::export_operation!(
    Component,
    exports::client_acme_receiving::quality::load_purchase_order_detail,
    wamn::node::types,
    wamn_postgres_statements::Connection::new(),
    quality::handle_detail,
    detail_codec
);
quality::approve_codec::export_operation!(
    Component,
    exports::client_acme_receiving::quality::approve_inspection,
    wamn::node::types,
    quality::handle_approve,
    approve_codec
);
create_codec::export_operation!(
    Component,
    exports::client_acme_receiving::quality::create_inspection,
    wamn::node::types,
    (),
    quality::handle_create,
    create_codec
);

fn emission(payload: String) -> Emission {
    Emission {
        payload,
        port: None,
    }
}

fn private_node_error(error: &AccessError) -> NodeError {
    let kind = error.kind();
    let code = match kind {
        AccessErrorType::InvalidInput => "invalid_input",
        AccessErrorType::Retry => "retry",
        AccessErrorType::Timeout => "timeout",
        AccessErrorType::NotFound
        | AccessErrorType::ConcurrencyConflict
        | AccessErrorType::ExclusionViolation
        | AccessErrorType::PermissionDenied
        | AccessErrorType::InternalError => "internal_error",
    };
    let detail = ErrorDetail {
        message: error.context().to_owned(),
        code: Some(code.to_owned()),
    };
    match kind {
        AccessErrorType::InvalidInput => NodeError::InvalidInput(detail),
        AccessErrorType::Retry | AccessErrorType::Timeout => NodeError::Retryable(detail),
        AccessErrorType::NotFound
        | AccessErrorType::ConcurrencyConflict
        | AccessErrorType::ExclusionViolation
        | AccessErrorType::PermissionDenied
        | AccessErrorType::InternalError => NodeError::Terminal(detail),
    }
}

fn access_detail(
    error: &AccessError,
    key: &str,
    operation: &str,
    not_found: Option<(&str, &str)>,
    expected_row_version: Option<i32>,
) -> Option<String> {
    match key {
        "field" => error.field().map(str::to_owned).or_else(|| {
            not_found
                .filter(|_| error.kind() == AccessErrorType::NotFound)
                .map(|(field, _)| field.to_owned())
        }),
        "id" => not_found
            .filter(|_| error.kind() == AccessErrorType::NotFound)
            .map(|(_, id)| id.to_owned()),
        "expected_row_version" => expected_row_version.map(|value| value.to_string()),
        "observed_row_version" => error.observed_row_version().map(|value| value.to_string()),
        "constraint" => error.constraint().map(str::to_owned),
        "operation" => Some(operation.to_owned()),
        _ => None,
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
        receipt_codec::validate(&input).map_err(|error| {
            NodeError::InvalidInput(ErrorDetail {
                message: error.context().to_owned(),
                code: Some("invalid_input".to_owned()),
            })
        })?;
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

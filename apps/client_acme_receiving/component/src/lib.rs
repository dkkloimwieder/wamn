#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! One package-grain component for executable Acme Receiving overlay operations.

use exports::client_acme_receiving::receiving::record_receipt::Guest as RecordReceipt;
use wamn::node::types::{Emission, ErrorDetail, NodeContext, NodeError};
use wamn_client_acme_receiving_data_access::{AccessError, AccessErrorKind};

mod get;
mod quality;
mod update;

use get::codec as get_codec;
use quality::{approve_codec, detail_codec};
use update::codec as update_codec;

mod create_codec {
    use super::exports::client_acme_receiving::quality::create_inspection as contract;

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wit/quality_create_inspection_codec.rs"
    ));
}

wit_bindgen::generate!({
    world: "client-acme-receiving:component/client-acme-receiving@3.0.0",
    inline: r#"
        package client-acme-receiving:component@3.0.0;

        world client-acme-receiving {
          import wamn:postgres/types@0.2.0;
          import wamn:postgres/statements@0.2.0;
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
        "../../../crates/execution/workflow/router/wit",
        "../../../crates/platform/runtime/wit/deps/wamn-postgres-0.2",
        "../generated/wit/deps/client-acme-receiving-purchase-order",
        "../generated/wit/deps/client-acme-receiving-quality",
        "../../wamn_receiving/generated/wit/deps/wamn-receiving-receiving",
        "../generated/wit/deps/client-acme-receiving-receiving",
    ],
    generate_all,
    async: true,
});

struct Component;

get::codec::export_operation!(
    Component,
    exports::client_acme_receiving::purchase_order::get,
    wamn::node::types,
    wamn_postgres_statements::Connection::new(),
    get::handle,
    get_codec
);
update::codec::export_operation!(
    Component,
    exports::client_acme_receiving::purchase_order::update,
    wamn::node::types,
    wamn_postgres_statements::Connection::new(),
    update::handle,
    update_codec
);
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
    wamn_postgres_statements::Connection::new(),
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
                .filter(|_| error.kind() == AccessErrorKind::NotFound)
                .map(|(field, _)| field.to_owned())
        }),
        "id" => not_found
            .filter(|_| error.kind() == AccessErrorKind::NotFound)
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

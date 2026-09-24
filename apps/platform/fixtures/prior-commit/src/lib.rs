//! Commits one independent SQL effect before invoking typed Receiving.

#[expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.61 emits Vec::from_raw_parts with equal length and capacity"
)]
mod bindings {
    wit_bindgen::generate!({
        world: "prior-commit",
        inline: r#"
            package wamn:prior-commit@0.1.0;

            world prior-commit {
              import wamn:postgres/client@0.1.0;
              import wamn-receiving:receiving/record-receipt@1.0.0;
              export wamn:node/async-handler@0.1.0;
            }
        "#,
        path: [
            "../../../../crates/execution/workflow/router/wit",
            "../../../../crates/platform/runtime/wit/deps/wamn-postgres",
            "../../../wamn_receiving/generated/wit/deps/wamn-receiving-receiving",
        ],
        generate_all,
        async: true,
    });
}

use bindings::exports::wamn::node::async_handler::{Emission, Guest, NodeContext, NodeError};
use bindings::wamn::node::types::ErrorDetail;
use bindings::wamn::postgres::client;
use bindings::wamn_receiving::receiving::record_receipt as receipt;

mod receipt_codec {
    use super::receipt as contract;

    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../wamn_receiving/generated/wit/receiving_record_receipt_codec.rs"
    ));
}

const COUNTER_SQL: &str = "UPDATE fresh_only_probe.counter SET count = count + 1";

struct Component;

impl Guest for Component {
    async fn run(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let affected = client::execute(COUNTER_SQL.to_owned(), Vec::new())
            .await
            .map_err(postgres_error)?;
        if affected != 1 {
            return Err(internal_error(
                "counter update did not affect exactly one row",
            ));
        }
        let typed = receipt_codec::decode(&input).map_err(|error| {
            NodeError::InvalidInput(ErrorDetail {
                message: error.context().to_owned(),
                code: Some("invalid_input".to_owned()),
            })
        })?;
        let result = receipt::run(context, typed).await?;
        Ok(Emission {
            payload: receipt_codec::encode(&result),
            port: None,
        })
    }
}

fn postgres_error(_: bindings::wamn::postgres::types::PgError) -> NodeError {
    internal_error("counter update failed")
}

fn internal_error(message: &str) -> NodeError {
    NodeError::Terminal(ErrorDetail {
        message: message.to_owned(),
        code: Some("internal_error".to_owned()),
    })
}

bindings::export!(Component with_types_in bindings);

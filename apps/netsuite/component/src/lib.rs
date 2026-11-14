//! Read settings and commit the pull checkpoint through the composed base.
#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits equal length and capacity"
)]
#![expect(unsafe_code, reason = "wit-bindgen generated FFI glue")]
wit_bindgen::generate!({
    world: "netsuite:component/netsuite@0.1.0",
    path: [
        "../../../wit/wamn-node",
        "../../target/wamn/wamn_integration/wit/deps/wamn-integration-setting",
        "../../target/wamn/netsuite/wit",
        "wit",
    ],
    generate_all,
    async: true,
});

use exports::netsuite::sync::finish;
use wamn::node::types::{ErrorDetail, NodeContext, NodeError};
use wamn_integration::setting::write;

struct Component;
export!(Component);

mod finish_operation {
    use super::finish as contract;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/netsuite/wit/sync_finish_operation.rs"
    ));
}

fn invalid_input(message: &str) -> NodeError {
    NodeError::InvalidInput(ErrorDetail {
        message: message.to_owned(),
        code: Some("invalid_input".into()),
    })
}

impl finish::Guest for Component {
    async fn run(
        context: NodeContext,
        input: Vec<finish::FinishItem>,
    ) -> Result<Vec<finish::FinishOutcome>, NodeError> {
        finish_operation::validate(&input).map_err(|error| invalid_input(error.context()))?;
        for item in &input {
            if let Ok(request) = &item.input {
                chrono::DateTime::parse_from_rfc3339(&request.due)
                    .map_err(|error| invalid_input(&error.to_string()))?;
            }
        }
        let mut failure = None;
        let rows = finish_operation::run(input, &mut failure, async |failure, request| {
            let rows = write::run(
                context.clone(),
                vec![write::WriteItem {
                    request_id: context.delivery_id.clone(),
                    input: Ok(write::WriteRequest {
                        idempotency_key: context.delivery_id.clone(),
                        key: "last_synced_at".into(),
                        value: wamn::node::types::JsonValue {
                            encoded_json: serde_json::json!(request.due).to_string(),
                        },
                    }),
                }],
            )
            .await
            .map_err(|error| {
                *failure = Some(error);
                finish::FinishError::InternalError
            })?;
            let [row]: [write::WriteOutcome; 1] = rows
                .try_into()
                .map_err(|_| finish::FinishError::InternalError)?;
            row.outcome.map_err(|error| match error {
                write::WriteError::Retry => finish::FinishError::Retry,
                write::WriteError::Timeout => finish::FinishError::Timeout,
                _ => finish::FinishError::InternalError,
            })?;
            Ok(finish::FinishResult { due: request.due })
        })
        .await;
        match failure {
            Some(error) => Err(error),
            None => Ok(rows),
        }
    }
}

#[cfg(test)]
#[path = "../../tests/mappings.rs"]
mod mappings;

//! Read settings and commit the pull checkpoint through the composed base.
#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits equal length and capacity"
)]

wit_bindgen::generate!({
    world: "netsuite:component/netsuite@0.1.0",
    path: [
        "../../../crates/execution/workflow/router/wit",
        "../../target/wamn/wamn_integration/wit/deps/wamn-integration-setting",
        "../../target/wamn/netsuite/wit",
        "wit",
    ],
    generate_all,
    async: true,
});

use exports::netsuite::sync::finish;
use wamn::node::types::{Emission, ErrorDetail, NodeContext, NodeError};
use wamn_integration::setting::write;

struct Component;
export!(Component);

impl exports::wamn::node::async_handler::Guest for Component {
    async fn run(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let config: serde_json::Value =
            serde_json::from_str(&context.config).map_err(|error| invalid(error.to_string()))?;
        match config["operation"].as_str() {
            Some("setting.read") => wamn_integration::setting::read::run_json(context, input).await,
            _ => Err(invalid("operation must be setting.read".into())),
        }
    }
}

fn invalid(message: String) -> NodeError {
    NodeError::Terminal(ErrorDetail {
        message,
        code: Some("invalid_config".into()),
    })
}

mod finish_codec {
    use super::finish as contract;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/netsuite/wit/sync_finish_codec.rs"
    ));
}

fn invalid_input(message: impl ToString) -> NodeError {
    NodeError::InvalidInput(ErrorDetail {
        message: message.to_string(),
        code: Some("invalid_input".into()),
    })
}

impl finish::Guest for Component {
    async fn run(
        context: NodeContext,
        input: Vec<finish::FinishItem>,
    ) -> Result<Vec<finish::FinishOutcome>, NodeError> {
        finish_codec::validate(&input).map_err(invalid_input)?;
        for item in &input {
            if let Ok(request) = &item.input {
                chrono::DateTime::parse_from_rfc3339(&request.due).map_err(invalid_input)?;
            }
        }
        let mut failure = None;
        let rows = finish_codec::run(input, &mut failure, async |failure, request| {
            let rows = write::run(
                context.clone(),
                vec![write::WriteItem {
                    request_id: context.delivery_id.clone(),
                    input: Ok(write::WriteRequest {
                        idempotency_key: context.delivery_id.clone(),
                        key: "last_synced_at".into(),
                        value: serde_json::json!(request.due).to_string(),
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

    async fn run_json(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let input = finish_codec::decode(&input).map_err(invalid_input)?;
        let rows = <Self as finish::Guest>::run(context, input).await?;
        Ok(Emission {
            payload: finish_codec::encode(&rows),
            port: None,
        })
    }
}

#[cfg(test)]
#[path = "../../tests/mappings.rs"]
mod mappings;

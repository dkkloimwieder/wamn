//! Forward setting calls to the composed base. Vendor behavior lives in the wirings.
#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits equal length and capacity"
)]

wit_bindgen::generate!({
    world: "netsuite:component/netsuite@0.1.0",
    path: [
        "../../../crates/execution/workflow/router/wit",
        "../../target/wamn/wamn_integration/wit/deps/wamn-integration-setting",
        "wit",
    ],
    generate_all,
    async: true,
});

use wamn::node::types::{Emission, ErrorDetail, NodeContext, NodeError};

struct Component;
export!(Component);

impl exports::wamn::node::async_handler::Guest for Component {
    async fn run(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let config: serde_json::Value =
            serde_json::from_str(&context.config).map_err(|error| invalid(error.to_string()))?;
        match config["operation"].as_str() {
            Some("setting.read") => wamn_integration::setting::read::run_json(context, input).await,
            Some("setting.write") => {
                wamn_integration::setting::write::run_json(context, input).await
            }
            _ => Err(invalid(
                "operation must be setting.read or setting.write".into(),
            )),
        }
    }
}

fn invalid(message: String) -> NodeError {
    NodeError::Terminal(ErrorDetail {
        message,
        code: Some("invalid_config".into()),
    })
}

#[cfg(test)]
#[path = "../../tests/mappings.rs"]
mod mappings;

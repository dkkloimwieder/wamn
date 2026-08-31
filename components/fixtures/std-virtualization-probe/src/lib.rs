#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! Standard-library guest proving build-time WASI virtualization behavior.

use exports::wamn::node::handler::{Emission, Guest, NodeContext, NodeError};
use wamn::connection::http::Request;
use wamn::node::types::ErrorDetail;

wit_bindgen::generate!({
    world: "http-request",
    path: "../../no-std/http-request/wit",
    generate_all,
});

/// A sentinel deliberately present in the native test process. The virtualized
/// guest must report it absent without relying on the host runtime to hide it.
const SENTINEL_KEY: &str = "WAMN_STD_VIRTUALIZATION_SENTINEL";

struct Component;

impl Guest for Component {
    fn run(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let input: serde_json::Value = serde_json::from_str(&input).map_err(|error| {
            invalid_input("invalid-json", format!("input is not JSON: {error}"))
        })?;
        match input.get("proof").and_then(serde_json::Value::as_str) {
            Some("environment") => Ok(environment_probe()),
            Some("connection") => connection_probe(&context),
            Some("panic") => {
                panic!("the std virtualization trap proof requested a deliberate guest panic");
            }
            _ => Err(invalid_input(
                "invalid-proof",
                "proof must be environment, connection, or panic",
            )),
        }
    }
}

fn environment_probe() -> Emission {
    Emission {
        payload: serde_json::json!({
            "sentinel-key": SENTINEL_KEY,
            "sentinel-visible": std::env::var_os(SENTINEL_KEY).is_some(),
        })
        .to_string(),
        port: None,
    }
}

fn connection_probe(context: &NodeContext) -> Result<Emission, NodeError> {
    let config: serde_json::Value = serde_json::from_str(&context.config)
        .map_err(|error| invalid_input("invalid-config", format!("config is not JSON: {error}")))?;
    let requirement = required_config(&config, "requirement")?;
    let method = required_config(&config, "method")?;
    let path_and_query = required_config(&config, "path-and-query")?;
    let response = wamn::connection::http::send(&Request {
        requirement,
        method,
        path_and_query,
        headers: Vec::new(),
        body: None,
        idempotency_key: None,
    })
    .map_err(|error| {
        NodeError::Terminal(ErrorDetail {
            message: format!("connection proof failed: {error:?}"),
            code: Some("connection-proof-failed".to_owned()),
        })
    })?;
    Ok(Emission {
        payload: serde_json::json!({"connection-status": response.status}).to_string(),
        port: None,
    })
}

fn required_config(config: &serde_json::Value, field: &str) -> Result<String, NodeError> {
    config
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid_input("invalid-config", format!("{field} must be a string")))
}

fn invalid_input(code: &str, message: impl Into<String>) -> NodeError {
    NodeError::InvalidInput(ErrorDetail {
        message: message.into(),
        code: Some(code.to_owned()),
    })
}

export!(Component);

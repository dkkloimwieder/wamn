//! Standard-library guest proving build-time WASI virtualization behavior.

use exports::wamn::node::handler::{Emission, Guest, NodeContext, NodeError};
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
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let input: serde_json::Value = serde_json::from_str(&input).map_err(|error| {
            NodeError::InvalidInput(ErrorDetail {
                message: format!("input is not JSON: {error}"),
                code: Some("invalid-json".to_owned()),
            })
        })?;
        if input.get("proof").and_then(serde_json::Value::as_str) == Some("panic") {
            panic!("the std virtualization trap proof requested a deliberate guest panic");
        }

        Ok(Emission {
            payload: serde_json::json!({
                "sentinel-key": SENTINEL_KEY,
                "sentinel-visible": std::env::var_os(SENTINEL_KEY).is_some(),
            })
            .to_string(),
            port: None,
        })
    }
}

export!(Component);

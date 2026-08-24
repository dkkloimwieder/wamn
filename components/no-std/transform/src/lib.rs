//! Ordinary transform palette component.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString as _};

#[path = "../../guest_runtime.rs"]
mod guest_runtime;

wit_bindgen::generate!({
    world: "transform",
    path: "wit",
    generate_all,
    std_feature,
});

use exports::wamn::node::handler::{Emission, Guest, NodeContext, NodeError};
use wamn::node::types::ErrorDetail;

struct Component;

impl Guest for Component {
    fn run(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let input = serde_json::from_str::<serde_json::Value>(&input)
            .map_err(|error| invalid_input(format!("input is not JSON: {error}")))?;
        let config = serde_json::from_str::<serde_json::Value>(&context.config)
            .map_err(|error| terminal("invalid-config", format!("config is not JSON: {error}")))?;
        let pointer = config
            .get("pointer")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| terminal("invalid-config", "pointer must be a string"))?;
        let value = input.pointer(pointer).ok_or_else(|| {
            terminal(
                "pointer-not-found",
                format!("JSON pointer {pointer:?} does not resolve"),
            )
        })?;
        let payload = serde_json::to_string(value).map_err(|error| {
            terminal("transform-failed", format!("result is not JSON: {error}"))
        })?;
        Ok(Emission {
            payload,
            port: None,
        })
    }
}

fn invalid_input(message: String) -> NodeError {
    NodeError::InvalidInput(ErrorDetail {
        message,
        code: Some("invalid-json".to_string()),
    })
}

fn terminal(code: &str, message: impl Into<String>) -> NodeError {
    NodeError::Terminal(ErrorDetail {
        message: message.into(),
        code: Some(code.to_string()),
    })
}

export!(Component);

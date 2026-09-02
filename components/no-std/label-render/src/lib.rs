//! Label-render palette component.
//!
//! A pure transform: the wiring's `template_id` parameter plus a field record
//! in, one ZPL label out. It imports only
//! `wamn:node`, so it holds no authority that leaves the host and its effect
//! projection is empty — which is what makes a wiring carrying it eligible for
//! the gate's effect-free-case path. Nothing here declares that; it follows
//! from the imports.
//!
//! The rendering itself lives in `label-template` so it can be gated by golden
//! vectors: a `no_std` cdylib guest has no test target (wamn-6i30). This file
//! is the WIT boundary and nothing else — parse, delegate, translate the error
//! exactly once.
//!
//! `template_id` is a **wiring parameter**, not an input field. Template choice
//! is authoring intent: a wirer picks "pallet label" once and the wiring then
//! declares honestly what it renders, so gate cases can pin golden output per
//! wiring. Taking it from the input would make every caller a template chooser
//! and the wiring's behaviour caller-dependent. It also puts the closed set in
//! the declaration's parameter schema as a typed `enum`, so an unknown
//! template refuses at gate/config-validation time rather than at first
//! invocation. The check below is defence in depth behind that, not the
//! primary gate.

#![no_std]

extern crate alloc;

use alloc::string::{String, ToString as _};

#[path = "../../guest_runtime.rs"]
mod guest_runtime;

wit_bindgen::generate!({
    world: "label-render",
    path: "wit",
    generate_all,
    std_feature,
});

use exports::wamn::node::handler::{Emission, Guest, NodeContext, NodeError};
use wamn::node::types::ErrorDetail;

struct Component;

impl Guest for Component {
    fn run(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let config = serde_json::from_str::<serde_json::Value>(&context.config)
            .map_err(|_| terminal("invalid_config", "config is not JSON"))?;
        let template_id = config
            .get("template_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| terminal("invalid_config", "template_id parameter must be a string"))?;
        let fields = serde_json::from_str::<serde_json::Value>(&input)
            .map_err(|_| invalid_input("input_not_json", "input is not JSON"))?;
        if !fields.is_object() {
            return Err(invalid_input("input_not_object", "input must be a JSON object"));
        }

        // The one translation of the implementation error into the WIT
        // vocabulary. Every render refusal is caller-supplied input, so they
        // all land on the invalid-input arm carrying the render kind's code.
        let zpl = label_template::render(template_id, &fields)
            .map_err(|error| invalid_input(error.code(), error.detail()))?;

        let payload = serde_json::to_string(&serde_json::json!({ "zpl": zpl }))
            .map_err(|_| terminal("render_not_encodable", "rendered label is not encodable"))?;
        Ok(Emission {
            payload,
            port: None,
        })
    }
}

fn invalid_input(code: &str, message: &str) -> NodeError {
    NodeError::InvalidInput(ErrorDetail {
        message: message.to_string(),
        code: Some(code.to_string()),
    })
}

fn terminal(code: &str, message: &str) -> NodeError {
    NodeError::Terminal(ErrorDetail {
        message: message.to_string(),
        code: Some(code.to_string()),
    })
}

export!(Component);

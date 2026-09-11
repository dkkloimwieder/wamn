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
//! **The output ENRICHES the input**: every field the record arrived with, plus
//! `zpl`. Edges carry payloads verbatim, so a node that replaced its payload
//! would destroy upstream context at its own hop — and a downstream node could
//! not recover it, because a wiring parameter can point at a field but cannot
//! resurrect one. A palette transform that means to NARROW the payload
//! declares itself a projection instead.
//!
//! **The payload on an edge is the route ENVELOPE** (RULED `wamn-362o.42`):
//! an array of items, each `{request_id, value}` or `{request_id, error}`,
//! exactly what the entry operation emits and what the route answers with.
//! The platform carries one value of one schema per edge and has no fan-out,
//! so a palette node applies itself to EACH ITEM'S VALUE and passes error
//! items through untouched; its ports declare `{"type": "array"}`,
//! byte-identical to the entry's, which is what the gate's digest rule
//! compares. Alternatives noted for future exploration in
//! `docs/exe-model.md`: a router fan-out delivering items one at a time, and
//! per-item outcome reporting for nodes whose work can fail per item.
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
        let mut envelope = serde_json::from_str::<serde_json::Value>(&input)
            .map_err(|_| invalid_input("input_not_json", "input is not JSON"))?;
        let items = envelope.as_array_mut().ok_or_else(|| {
            invalid_input(
                "input_not_envelope",
                "input must be the route envelope: a JSON array of items",
            )
        })?;
        for item in items.iter_mut() {
            // An error item carries no value to render; it passes through so
            // the caller still sees every request answered.
            let Some(fields) = item.get_mut("value") else {
                continue;
            };
            let Some(record) = fields.as_object_mut() else {
                return Err(invalid_input(
                    "item_value_not_object",
                    "an item's value must be a JSON object",
                ));
            };
            // The one translation of the implementation error into the WIT
            // vocabulary. Every render refusal is caller-supplied input, so
            // they all land on the invalid-input arm carrying the render
            // kind's code.
            let zpl = label_template::render(template_id, fields)
                .map_err(|error| invalid_input(error.code(), error.detail()))?;
            // ENRICHING, not replacing. An edge carries a payload verbatim, so
            // a node that emitted `{zpl}` alone would destroy every upstream
            // fact at this hop — the movement id a downstream node needs for
            // its object key would simply be gone, and no wiring parameter
            // could recover it. A pure transform therefore ADDS its output to
            // the record it was given; a node that means to narrow the payload
            // says so by being a projection.
            //
            // `zpl` overwrites an inbound member of the same name deliberately:
            // this node's own output is the authority on what it rendered.
            let record = fields
                .as_object_mut()
                .expect("the value was checked to be an object above");
            record.insert("zpl".to_string(), serde_json::Value::String(zpl));
        }
        let payload = serde_json::to_string(&envelope)
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

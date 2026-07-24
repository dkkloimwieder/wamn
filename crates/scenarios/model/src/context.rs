//! Optional invocation context carried by a node scenario.

use serde::{Deserialize, Serialize};

/// A scenario's node invocation context.
///
/// The serialized fields intentionally match the frozen `wamn:node`
/// `run-context`; invocation adapters translate this product type at their
/// boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RunContext {
    pub run_id: String,
    pub flow_id: String,
    pub flow_version: u32,
    pub node_id: String,
    pub attempt: u32,
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracestate: Option<String>,
    /// The node's JSON configuration document.
    pub config: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn run_context_preserves_the_frozen_wire_shape() {
        let context = RunContext {
            run_id: "run-1".into(),
            flow_id: "flow".into(),
            flow_version: 2,
            node_id: "node".into(),
            attempt: 3,
            idempotency_key: "run-1:node:3".into(),
            deadline_ms: Some(42),
            traceparent: None,
            tracestate: None,
            config: "{}".into(),
        };
        assert_eq!(
            serde_json::to_value(context).unwrap(),
            json!({
                "run-id": "run-1",
                "flow-id": "flow",
                "flow-version": 2,
                "node-id": "node",
                "attempt": 3,
                "idempotency-key": "run-1:node:3",
                "deadline-ms": 42,
                "config": "{}"
            })
        );
    }
}

//! `sample-echo` — the reference custom node (5.4) and the frozen-contract
//! conformance fixture. Pure logic against the SDK trait; the scaffolding
//! macro at the bottom is the entire componentization (the 5.13 promise).
//!
//! Behavior (driven by the `nodebench --mode sample` gate):
//! - input `{"fail": "<variant>"}` returns that taxonomy variant
//!   (`retryable` / `rate-limited` / `terminal` / `invalid-input` /
//!   `cancelled`), proving the scaffolding's error conversion end-to-end;
//! - config `{"port": "p"}` emits on port `p` (absent = `main`), proving
//!   port mapping;
//! - anything else echoes `{"echo": <input>, "node": .., "attempt": ..}`.

use serde_json::{Value, json};
use wamn_node_sdk::{Emission, ErrorDetail, Node, NodeCtx, NodeError, RateLimitDetail, RunContext};

#[derive(Default)]
struct SampleEcho;

impl Node for SampleEcho {
    fn run(
        &self,
        _ctx: &mut dyn NodeCtx,
        run: &RunContext<'_>,
        input: &Value,
    ) -> Result<Emission, NodeError> {
        if let Some(variant) = input.get("fail").and_then(Value::as_str) {
            return Err(match variant {
                "retryable" => {
                    NodeError::Retryable(ErrorDetail::coded("SAMPLE_RETRY", "sample retryable"))
                }
                "rate-limited" => NodeError::RateLimited(RateLimitDetail {
                    detail: ErrorDetail::coded("SAMPLE_429", "sample rate-limited"),
                    retry_after_ms: Some(1500),
                    target_host: Some("sample.example".to_string()),
                }),
                "invalid-input" => NodeError::InvalidInput(ErrorDetail::coded(
                    "SAMPLE_INVALID",
                    "sample invalid input",
                )),
                "cancelled" => NodeError::Cancelled,
                // "terminal" and anything unknown: permanent.
                other => NodeError::Terminal(ErrorDetail::coded(
                    "SAMPLE_TERMINAL",
                    format!("sample terminal ({other})"),
                )),
            });
        }
        let out = json!({"echo": input, "node": run.node_id, "attempt": run.attempt});
        Ok(match run.config.get("port").and_then(Value::as_str) {
            Some(p) => Emission::on(out, p),
            None => Emission::main(out),
        })
    }
}

wamn_node_guest::export_node!(SampleEcho);

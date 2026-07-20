//! The wamn-bd5 credential-reading custom node (5.6) — the live-path proof that
//! the runner->serve-node hop carries payloads AND enforces the per-invocation
//! credential grant at the REAL WIT boundary.
//!
//! Behavior (driven by config; the `nodeinvoke` gate seeds it):
//! - echoes the input payload back (payload round-trip);
//! - `config.probe` names a credential to `get` — the flow declared it, so the
//!   serve-node host granted it, so this resolves (`ok:<secret>`);
//! - `config.forbidden` names a credential the flow did NOT declare, so it was
//!   NOT granted — `get` returns `not-granted` at the boundary (the credprobe
//!   negative, now on the live path);
//! - it imports `wamn:node/credentials` DIRECTLY and has NO way to self-grant
//!   (no `wamn:runner/credentials`), so the host grant is the only gate.

wit_bindgen::generate!({
    world: "node-cred",
    path: "wit",
    generate_all,
});

use exports::wamn::node::handler::Guest;
use serde_json::{Value, json};
use wamn::node::credentials::{self, CredentialError};
use wamn::node::types::{Emission, ErrorDetail, NodeError, Payload, RunContext};

struct Component;

fn terminal(code: &str, msg: String) -> NodeError {
    NodeError::Terminal(ErrorDetail {
        message: msg,
        code: Some(code.to_string()),
        data: None,
    })
}

fn invalid(msg: String) -> NodeError {
    NodeError::InvalidInput(ErrorDetail {
        message: msg,
        code: Some("SCHEMA_MISMATCH".to_string()),
        data: None,
    })
}

/// Resolve one credential name through the host vault, tagging the outcome so
/// the gate can assert on the exact variant (mirrors the cred-probe fixture).
fn read_cred(name: &str) -> String {
    match credentials::get(name) {
        Ok(secret) => format!("ok:{secret}"),
        Err(CredentialError::NotGranted) => "err:not-granted".to_string(),
        Err(CredentialError::NotFound) => "err:not-found".to_string(),
        Err(CredentialError::Unavailable) => "err:unavailable".to_string(),
    }
}

impl Guest for Component {
    fn run(ctx: RunContext, input: Payload) -> Result<Emission, NodeError> {
        let inline = match input {
            Payload::Inline(s) => s,
            Payload::Streamed(_) => {
                return Err(terminal(
                    "streamed-payload-unsupported",
                    "streamed payloads land with the payload store (5.10)".to_string(),
                ));
            }
        };
        let input_v: Value = serde_json::from_str(&inline).unwrap_or(Value::Null);
        let cfg: Value =
            serde_json::from_str(&ctx.config).map_err(|e| invalid(format!("bad config: {e}")))?;

        let probe = cfg
            .get("probe")
            .and_then(Value::as_str)
            .map(read_cred)
            .unwrap_or_else(|| "none".to_string());
        let forbidden = cfg
            .get("forbidden")
            .and_then(Value::as_str)
            .map(read_cred)
            .unwrap_or_else(|| "none".to_string());

        let out = json!({
            "echo": input_v,
            "node": ctx.node_id,
            "attempt": ctx.attempt,
            "probe": probe,
            "forbidden": forbidden,
        });
        // Frozen 0.1: run returns an emission; absent port = "main".
        Ok(Emission {
            payload: Payload::Inline(out.to_string()),
            port: None,
        })
    }
}

export!(Component);

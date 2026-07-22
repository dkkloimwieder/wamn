//! 11.5 — the custom-node test gate: user-supplied test cases run against the
//! pure `wamn:node` `run(ctx, input)` contract as a PUBLISH gate. A failing case
//! REFUSES the publish (a [`TestGateError`] lifted to `anyhow` → non-zero exit →
//! nothing reaches the registry), exactly like the dependency-allowlist and
//! import-lint stages before it.
//!
//! The executor instantiates the JUST-BUILT artifact bytes under the frozen
//! `wamn:node` world host-side (the production [`ServeNode`] host the runner
//! uses), synthesizes a fixed `ctx` per case (the `f2invoke` gate is the literal
//! template), invokes, and folds the case's expectation against the
//! `NodeInvokeResponse`. Empty vault / no signing key / deny-all egress — a case
//! that reaches for a credential or egress is refused at the real WIT boundary.
//!
//! ## Case vocabulary — RECONCILED to wamn-testkit (wamn-gyt)
//!
//! The case/assertion vocabulary is the canonical `wamn-testkit` crate's: the
//! file envelope [`CaseFile`] (`wamn_testkit::NodeCaseFile`) parses `cases.json`,
//! each [`NodeCase`] lowers to a `wamn_testkit::TestCase`
//! ([`NodeCase::into_test_case`]), and the outcome is decided by
//! [`wamn_testkit::evaluate`] over a [`Captured`] bundle built from the
//! `NodeInvokeResponse`. Only the EXECUTION glue ([`run_cases`] — `ServeNode`
//! instantiation, wire mapping, and the typed [`TestGateError`] refusal) stays in
//! the builder; the vocabulary is shared with the 828 catalog-jsonb store and the
//! `wamn-gates testkitbench` gate. The 7se `grant` rides the [`NodeCase`] and is
//! read off it here when building the request (a `TestCase` carries no grant).

use std::sync::Arc;

use anyhow::Context as _;

use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_credentials::WamnCredentials;
use wamn_host::serve_node::{self, ServeNode, ServeNodeAuthn};
use wamn_node_invoke::{
    NodeInvokeRequest, NodeInvokeResponse, WireNodeError, WirePayload, WireRunContext,
};
use wamn_testkit::{Captured, NodeCase, NodeErrorKind, evaluate};

// The reconciled case vocabulary is wamn-testkit's; re-exported under the
// builder's historical `CaseFile` name so `build.rs` and the hermetic
// `wamn-gates testgate` import surface (`CaseFile::from_json`, `run_cases`) are
// unchanged.
pub use wamn_testkit::NodeCaseFile as CaseFile;

// ---------------------------------------------------------------------------
// Refusal: a typed error naming every failing case (mirrors AllowlistError)
// ---------------------------------------------------------------------------

/// One case that did not meet its expectation: the case name and a human
/// expected-vs-got detail.
#[derive(Debug, Clone)]
pub struct CaseFailure {
    name: String,
    detail: String,
}

impl CaseFailure {
    fn new(name: &str, detail: String) -> Self {
        CaseFailure {
            name: name.to_string(),
            detail,
        }
    }

    /// The failing case name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A 11.5 test-gate refusal: one or more cases failed against the built
/// artifact, so the publish is REFUSED. `Display` names each failing case with
/// its expected-vs-got detail (the AllowlistError shape).
#[derive(Debug)]
pub struct TestGateError {
    failures: Vec<CaseFailure>,
}

impl TestGateError {
    /// The names of every failing case (sorted by first-seen order).
    pub fn failed_case_names(&self) -> Vec<&str> {
        self.failures.iter().map(CaseFailure::name).collect()
    }
}

impl std::fmt::Display for TestGateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "custom-node test gate (11.5): {} case(s) FAILED against the built \
             artifact — the publish is REFUSED (nothing is pushed):",
            self.failures.len()
        )?;
        for fail in &self.failures {
            write!(f, "\n  - {}: {}", fail.name, fail.detail)?;
        }
        Ok(())
    }
}

impl std::error::Error for TestGateError {}

// ---------------------------------------------------------------------------
// Wire mapping: request in, captured facts out
// ---------------------------------------------------------------------------

/// The invocation envelope for a case: a FIXED ctx (`f2invoke` style) with the
/// case's input, config (default `{}`), and the 7se grant riding the request.
/// Everything else is fixed.
fn build_request(case: &NodeCase) -> NodeInvokeRequest {
    let config = case
        .config
        .as_ref()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "{}".to_string());
    NodeInvokeRequest {
        ctx: WireRunContext {
            run_id: "test-gate".to_string(),
            flow_id: "test-gate".to_string(),
            flow_version: 1,
            node_id: "case".to_string(),
            attempt: 0,
            idempotency_key: "test-gate:case".to_string(),
            deadline_ms: None,
            traceparent: None,
            tracestate: None,
            config,
        },
        input: WirePayload::Inline(case.input.to_string()),
        grant: case.grant.clone().unwrap_or_default(),
    }
}

/// Build the pure [`Captured`] fact bundle from an invocation response: a success
/// emission fills `node_output` (parsed inline JSON) + `node_port` (absent → the
/// literal `main`); an error fills `node_error` with the frozen taxonomy kind.
/// Mirrors the `wamn-gates testkitbench` capture so both gates decide over
/// identical facts.
fn capture(resp: &NodeInvokeResponse) -> Captured {
    match resp {
        NodeInvokeResponse::Ok(em) => {
            let node_output = match &em.payload {
                WirePayload::Inline(s) => serde_json::from_str(s).ok(),
            };
            Captured {
                node_output,
                node_port: Some(em.port.clone().unwrap_or_else(|| "main".into())),
                ..Default::default()
            }
        }
        NodeInvokeResponse::Err(e) => Captured {
            node_error: Some(wire_error_kind(e)),
            ..Default::default()
        },
    }
}

fn wire_error_kind(e: &WireNodeError) -> NodeErrorKind {
    match e {
        WireNodeError::Retryable(_) => NodeErrorKind::Retryable,
        WireNodeError::RateLimited(_) => NodeErrorKind::RateLimited,
        WireNodeError::Terminal(_) => NodeErrorKind::Terminal,
        WireNodeError::InvalidInput(_) => NodeErrorKind::InvalidInput,
        WireNodeError::Cancelled => NodeErrorKind::Cancelled,
    }
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

/// Run every case in `cases` against the built artifact `wasm` under the frozen
/// `wamn:node` world, refusing the publish if any case fails. The ONE runner
/// both the builder stage and the hermetic `wamn-gates testgate` gate call — a
/// case failure returns a [`TestGateError`] (lifted to `anyhow`), never a push.
///
/// MUTATION target: an early `return Ok(())` skips the gate entirely — killed by
/// the hermetic negative arm (`wamn-gates testgate`), which requires this to
/// return `Err` for a deliberately-wrong expectation.
pub async fn run_cases(wasm: &[u8], cases: &CaseFile) -> anyhow::Result<()> {
    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);

    // A world-node host posture (f2invoke): empty vault (no credentials), no
    // signing key (the direct in-process `.invoke()` is admitted), deny-all
    // egress. A case that reaches for a credential or a host is refused at the
    // real WIT boundary and surfaces as its case outcome.
    let serve = ServeNode::new(
        &engine,
        wasm,
        Arc::new(WamnCredentials::empty()),
        serve_node::DEFAULT_NODE_ID,
        "default",
        Arc::from([]),
        ServeNodeAuthn {
            require_signing_key: false,
            max_signature_age_secs: None,
        },
    )
    .await;
    let serve = match serve {
        Ok(s) => s,
        Err(e) => {
            ticker.abort();
            return Err(e).context("test gate: warm-instantiate the node under test");
        }
    };

    println!(
        "test gate: running {} case(s) against the built artifact",
        cases.cases.len()
    );
    let mut failures = Vec::new();
    for case in &cases.cases {
        let resp = serve.invoke(build_request(case)).await;
        let captured = capture(&resp);
        // Lower the compact node case to the canonical vocabulary and let the
        // pure evaluator decide (grant is not surfaced — it rode the request).
        let outcome = evaluate(&case.clone().into_test_case(), &captured);
        if outcome.passed() {
            println!("  test-gate PASS: {}", case.name);
        } else {
            let detail = outcome
                .failures()
                .filter_map(|r| r.detail.clone())
                .collect::<Vec<_>>()
                .join("; ");
            println!("  test-gate FAIL: {} — {}", case.name, detail);
            failures.push(CaseFailure::new(&case.name, detail));
        }
    }
    ticker.abort();

    if failures.is_empty() {
        Ok(())
    } else {
        Err(TestGateError { failures }.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use wamn_node_invoke::{WireEmission, WireErrorDetail};
    use wamn_testkit::{MatchMode, NodeExpect, NodeOk};

    fn node_case(grant: Option<Vec<String>>, config: Option<Value>) -> NodeCase {
        NodeCase {
            name: "c".into(),
            input: json!({"hold": {"material": "x"}}),
            config,
            grant,
            expect: NodeExpect::Ok(NodeOk {
                value: json!({}),
                match_mode: MatchMode::Exact,
                port: None,
            }),
        }
    }

    /// The 7se grant delta rides the request; config defaults to `{}`, and the
    /// input is carried inline.
    #[test]
    fn build_request_carries_grant_config_and_input() {
        let req = build_request(&node_case(
            Some(vec!["notify-token".into()]),
            Some(json!({"mode": "noop"})),
        ));
        assert_eq!(req.grant, vec!["notify-token".to_string()]);
        assert_eq!(req.ctx.config, r#"{"mode":"noop"}"#);
        assert_eq!(
            req.input,
            WirePayload::Inline(r#"{"hold":{"material":"x"}}"#.to_string())
        );

        // Absent grant/config → an empty grant and a `{}` config.
        let req = build_request(&node_case(None, None));
        assert!(req.grant.is_empty());
        assert_eq!(req.ctx.config, "{}");
    }

    /// The response → captured-facts mapping: an Ok fills node_output (parsed) +
    /// node_port (absent → `main`); an Err fills node_error with the taxonomy kind.
    #[test]
    fn capture_maps_ok_emission_and_error() {
        let ok = capture(&NodeInvokeResponse::Ok(WireEmission {
            payload: WirePayload::Inline(json!({"recommended": "reject"}).to_string()),
            port: None,
        }));
        assert_eq!(ok.node_output, Some(json!({"recommended": "reject"})));
        assert_eq!(ok.node_port.as_deref(), Some("main"));
        assert!(ok.node_error.is_none());

        let err = capture(&NodeInvokeResponse::Err(WireNodeError::InvalidInput(
            WireErrorDetail {
                message: "bad".into(),
                code: None,
                data: None,
            },
        )));
        assert_eq!(err.node_error, Some(NodeErrorKind::InvalidInput));
        assert!(err.node_output.is_none());
    }

    /// The refusal names every failing case with its expected-vs-got detail.
    #[test]
    fn test_gate_error_names_every_failing_case() {
        let err = TestGateError {
            failures: vec![
                CaseFailure::new("severe-rejects", "expected ok, got error terminal".into()),
                CaseFailure::new("bad-decimal", "expected error invalid-input, got ok".into()),
            ],
        };
        assert_eq!(
            err.failed_case_names(),
            vec!["severe-rejects", "bad-decimal"]
        );
        let shown = err.to_string();
        assert!(shown.contains("2 case(s) FAILED"));
        assert!(shown.contains("REFUSED"));
        assert!(shown.contains("severe-rejects: expected ok, got error terminal"));
        assert!(shown.contains("bad-decimal: expected error invalid-input, got ok"));
    }
}

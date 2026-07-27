//! `testgate` — the 11.5 custom-node test-gate publish proof.
//!
//! The 11.5 builder stage runs a node's user-supplied `cases.json` against the
//! JUST-BUILT artifact (under the frozen `wamn:node` world) as a PUBLISH gate: a
//! failing case REFUSES the publish, so nothing reaches the registry. This gate
//! proves the node-level contract HERMETICALLY through the shared node runtime,
//! independently of the deployable builder:
//!
//! - a POSITIVE arm — the disposition node's real `cases.json` (the transcribed
//!   `#[cfg(test)]` matrix) all PASS against the compiled artifact; and
//! - a NEGATIVE arm — a deliberately-wrong `cases-refusal-fixture.json` (a severe
//!   moisture exceedance WRONGLY expected to `accept`) is REFUSED with the typed
//!   `TestGateError`, naming the failing case.
//!
//! `run_cases` takes only wasm bytes + cases and does NO registry I/O — so a
//! typed refusal returned from it (as the negative arm shows) is proof the
//! publish is refused BEFORE any push side-effect: `build.rs::run` `?`-propagates
//! that `Err` before the push block ever runs.
//!
//! The two `cases.json` fixtures are `include_str!`d from the disposition-node
//! crate (the same files the builder-svc image bakes for the in-cluster
//! `deploy/gates/f2-testgate-job.yaml`), so this gate cannot drift from them.
//! Only the compiled artifact comes from a path (`--node`), exactly like
//! `f2invoke`: in-cluster it runs from the gates image against
//! `/bench/disposition-node.wasm`.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::Args;

use wamn_node_invoke::{
    NodeInvokeRequest, NodeInvokeResponse, WireNodeError, WirePayload, WireRunContext,
};
use wamn_node_runtime::{DEFAULT_NODE_ID, NodeRuntime, NodeRuntimeConfig};
use wamn_scenario_model::{Captured, NodeCase, NodeErrorKind, evaluate};

type CaseFile = wamn_scenario_model::NodeCaseFile;

#[derive(Debug)]
struct TestGateError {
    failures: Vec<(String, String)>,
}

impl TestGateError {
    fn failed_case_names(&self) -> Vec<&str> {
        self.failures
            .iter()
            .map(|(name, _detail)| name.as_str())
            .collect()
    }
}

impl std::fmt::Display for TestGateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "custom-node test gate: {} case(s) failed",
            self.failures.len()
        )?;
        for (name, detail) in &self.failures {
            write!(formatter, "\n  - {name}: {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for TestGateError {}

fn request(case: &NodeCase) -> NodeInvokeRequest {
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
            config: case
                .config
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "{}".to_string()),
            context: "{}".to_string(),
        },
        input: WirePayload::Inline(case.input.to_string()),
        grant: case.grant.clone().unwrap_or_default(),
    }
}

fn captured(response: &NodeInvokeResponse) -> Captured {
    match response {
        NodeInvokeResponse::Ok(emission) => Captured {
            node_output: emission
                .payload
                .inline()
                .and_then(|value| serde_json::from_str(value).ok()),
            node_port: Some(emission.port.clone().unwrap_or_else(|| "main".to_string())),
            ..Default::default()
        },
        NodeInvokeResponse::Err(error) => Captured {
            node_error: Some(match error {
                WireNodeError::Retryable(_) => NodeErrorKind::Retryable,
                WireNodeError::RateLimited(_) => NodeErrorKind::RateLimited,
                WireNodeError::Terminal(_) => NodeErrorKind::Terminal,
                WireNodeError::InvalidInput(_) => NodeErrorKind::InvalidInput,
                WireNodeError::Cancelled => NodeErrorKind::Cancelled,
            }),
            ..Default::default()
        },
    }
}

async fn run_cases(wasm: &[u8], cases: &CaseFile) -> anyhow::Result<()> {
    let engine = wamn_runtime::build_engine(&[])?;
    let ticker = wamn_runtime::spawn_epoch_ticker(&engine, wamn_runtime::DEFAULT_EPOCH_TICK);
    let runtime = NodeRuntime::instantiate(
        &engine,
        wasm,
        NodeRuntimeConfig::deny_all(DEFAULT_NODE_ID, "default"),
    )
    .await
    .context("warm-instantiate node under test")?;

    let mut failures = Vec::new();
    for case in &cases.cases {
        let response = runtime.invoke(request(case)).await;
        let outcome = evaluate(&case.clone().into_test_case(), &captured(&response));
        if !outcome.passed() {
            let detail = outcome
                .failures()
                .filter_map(|result| result.detail.clone())
                .collect::<Vec<_>>()
                .join("; ");
            failures.push((case.name.clone(), detail));
        }
    }
    ticker.abort();

    if failures.is_empty() {
        Ok(())
    } else {
        Err(TestGateError { failures }.into())
    }
}

/// The disposition node's real cases and the deliberately-wrong refusal fixture,
/// baked from the crate so the gate tracks them exactly.
const GOOD_CASES: &str = include_str!("../../../components/samples/disposition-node/cases.json");
const BAD_CASES: &str =
    include_str!("../../../components/samples/disposition-node/cases-refusal-fixture.json");

/// The one case the refusal fixture must name in its typed refusal.
const REFUSAL_CASE_NAME: &str = "severe-recommendation-WRONGLY-expects-accept";

#[derive(Debug, Args)]
pub struct TestGateArgs {
    /// The compiled disposition-recommendation node
    /// (`components/samples/disposition-node`, built for wasm32-wasip2).
    #[arg(long, default_value = "/bench/disposition-node.wasm")]
    pub node: PathBuf,
}

pub async fn run(args: TestGateArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    println!("# wamn-gates testgate — 11.5 custom-node test-gate publish proof (hermetic)");
    println!("# claim: a node's cases.json PASSES the publish gate; a deliberately-wrong");
    println!("#        expectation REFUSES it (typed TestGateError) before any push.");

    let wasm = std::fs::read(&args.node)
        .with_context(|| format!("read disposition node {}", args.node.display()))?;

    let good = CaseFile::from_json(GOOD_CASES).context("parse the good cases.json")?;
    let bad = CaseFile::from_json(BAD_CASES).context("parse the refusal fixture")?;

    let mut pass = true;

    // POSITIVE — the real cases.json all pass against the built artifact.
    println!("\n## positive — the disposition node's cases.json passes the gate");
    match run_cases(&wasm, &good).await {
        Ok(()) => println!(
            "    PASS: all {} case(s) passed — the publish proceeds",
            good.cases.len()
        ),
        Err(e) => {
            println!("    FAIL: the node's own cases.json did not pass: {e}");
            pass = false;
        }
    }

    // NEGATIVE — a deliberately-wrong expectation refuses with the typed error,
    // naming the case. The deployed builder Job separately proves that this
    // refusal prevents the registry push.
    println!("\n## negative — a deliberately-wrong expectation REFUSES the publish");
    match run_cases(&wasm, &bad).await {
        Ok(()) => {
            println!("    FAIL: a wrong expectation was ADMITTED — the publish gate is open");
            pass = false;
        }
        Err(e) => match e.downcast_ref::<TestGateError>() {
            Some(tge) if tge.failed_case_names().contains(&REFUSAL_CASE_NAME) => {
                println!(
                    "    PASS: refused with the typed TestGateError, naming {REFUSAL_CASE_NAME:?} — \
                    the node-level contract rejects the case"
                );
            }
            Some(tge) => {
                println!(
                    "    FAIL: refused, but the failing case(s) {:?} did not include {REFUSAL_CASE_NAME:?}",
                    tge.failed_case_names()
                );
                pass = false;
            }
            None => {
                println!("    FAIL: refused, but NOT with the typed TestGateError: {e}");
                pass = false;
            }
        },
    }

    println!("\ntestgate complete — overall PASS: {pass}");
    if !pass {
        bail!("11.5 testgate failed: the custom-node test-gate did not hold");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_node_manifest::{NodeManifest, RecoveryClass};

    /// The compiled disposition node, built from the components workspace. Absent
    /// = these wasm-driven checks SKIP (the pure test_gate units in wamn-builder
    /// cover the matching logic without a build); build it to exercise them:
    /// `cd components && cargo build --release --target wasm32-wasip2 -p disposition-node`.
    const WASM_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../components/target/wasm32-wasip2/release/disposition_node.wasm"
    );

    fn wasm() -> Option<Vec<u8>> {
        match std::fs::read(WASM_PATH) {
            Ok(bytes) => Some(bytes),
            Err(_) => {
                eprintln!("SKIPPED: {WASM_PATH} absent — build the disposition-node wasm first");
                None
            }
        }
    }

    fn custom_manifest(extra: &str) -> NodeManifest {
        NodeManifest::from_json(&format!(
            r#"{{"schema-version":"0.1","node-type":"custom","name":"Custom","version":"0.1.0","contract":"0.1.0"{extra}}}"#
        ))
        .expect("custom manifest parses")
    }

    /// T-NR control: absence is the dangerous custom-node default, never replay.
    #[test]
    fn t_nr_custom_manifest_without_purity_is_never_replay() {
        let interface = custom_manifest("")
            .resolved_interface()
            .expect("valid manifest resolves");
        assert_eq!(interface.recovery_class, RecoveryClass::NeverReplay);
    }

    /// Identity gate: both the resolved interface and component digest are pins.
    #[test]
    fn component_identity_changes_with_interface_or_digest() {
        let base = custom_manifest("")
            .resolved_component(format!("sha256:{}", "1".repeat(64)))
            .expect("identity resolves");
        let changed_interface = custom_manifest(r#","output-ports":["main","branch"]"#)
            .resolved_component(format!("sha256:{}", "1".repeat(64)))
            .expect("identity resolves");
        let changed_digest = custom_manifest("")
            .resolved_component(format!("sha256:{}", "2".repeat(64)))
            .expect("identity resolves");

        assert_ne!(base.identity_hash(), changed_interface.identity_hash());
        assert_ne!(base.identity_hash(), changed_digest.identity_hash());
    }

    /// PASS: the real cases.json passes against the compiled artifact.
    #[tokio::test]
    async fn real_cases_pass_against_the_compiled_node() {
        let Some(bytes) = wasm() else { return };
        let good = CaseFile::from_json(GOOD_CASES).expect("good cases parse");
        run_cases(&bytes, &good)
            .await
            .expect("the disposition node's own cases.json must pass");
    }

    /// VALUE-MISMATCH fail: the refusal fixture refuses with the typed error,
    /// naming the deliberately-wrong case.
    #[tokio::test]
    async fn value_mismatch_refuses_with_the_typed_error() {
        let Some(bytes) = wasm() else { return };
        let bad = CaseFile::from_json(BAD_CASES).expect("bad cases parse");
        let err = run_cases(&bytes, &bad)
            .await
            .expect_err("a wrong expectation must refuse");
        let tge = err
            .downcast_ref::<TestGateError>()
            .expect("the refusal is a typed TestGateError");
        assert!(tge.failed_case_names().contains(&REFUSAL_CASE_NAME));
    }

    /// ERROR-VARIANT fail: a case expecting the WRONG taxonomy variant (a
    /// malformed input yields invalid-input, but the case expects terminal)
    /// refuses.
    #[tokio::test]
    async fn wrong_error_variant_refuses() {
        let Some(bytes) = wasm() else { return };
        let cases = CaseFile::from_json(
            r#"{"cases":[{
              "name":"bad-decimal-wrongly-expects-terminal",
              "input":{"hold":{"material":"x","moisture_pct":"abc","moisture_max_pct":"5.00"},"decision":"accept"},
              "expect":{"error":"terminal"}
            }]}"#,
        )
        .expect("parses");
        let err = run_cases(&bytes, &cases)
            .await
            .expect_err("a wrong error variant must refuse");
        assert!(err.downcast_ref::<TestGateError>().is_some());
    }

    /// PORT-MISMATCH fail: a case pinning a non-main port the node never emits on
    /// refuses even though the value matches.
    #[tokio::test]
    async fn wrong_port_refuses() {
        let Some(bytes) = wasm() else { return };
        let cases = CaseFile::from_json(
            r#"{"cases":[{
              "name":"reject-wrongly-pins-a-branch-port",
              "input":{"hold":{"material":"resin-A","moisture_pct":"12.00","moisture_max_pct":"5.00"},"decision":"accept"},
              "expect":{"ok":{"value":{"recommendation":"reject","confidence":"0.80","matched":false},"match":"exact","port":"reject-branch"}}
            }]}"#,
        )
        .expect("parses");
        let err = run_cases(&bytes, &cases)
            .await
            .expect_err("a wrong port must refuse even when the value matches");
        assert!(err.downcast_ref::<TestGateError>().is_some());
    }
}

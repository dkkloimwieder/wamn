//! `testgate` — the 11.5 custom-node test-gate publish proof.
//!
//! The 11.5 builder stage runs a node's user-supplied `cases.json` against the
//! JUST-BUILT artifact (under the frozen `wamn:node` world) as a PUBLISH gate: a
//! failing case REFUSES the publish, so nothing reaches the registry. This gate
//! proves that enforcement HERMETICALLY, driving the builder's ONE test-gate
//! runner (`wamn_builder::test_gate::run_cases`) — the exact fn `build.rs::run`
//! calls after the import lint and BEFORE any OCI push — in-process:
//!
//! - a POSITIVE arm — the disposition node's real `cases.json` (the transcribed
//!   `#[cfg(test)]` matrix) all PASS against the compiled artifact; and
//! - a NEGATIVE arm — a deliberately-wrong `cases-refusal-fixture.json` (a severe
//!   moisture exceedance WRONGLY expected to `accept`) is REFUSED with the typed
//!   [`TestGateError`], naming the failing case.
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

use wamn_builder::test_gate::{CaseFile, TestGateError, run_cases};

/// The disposition node's real cases and the deliberately-wrong refusal fixture,
/// baked from the crate so the gate tracks them exactly.
const GOOD_CASES: &str = include_str!("../../../components/samples/disposition-node/cases.json");
const BAD_CASES: &str =
    include_str!("../../../components/samples/disposition-node/cases-refusal-fixture.json");

/// The one case the refusal fixture must name in its typed refusal.
const REFUSAL_CASE_NAME: &str = "severe-moisture-WRONGLY-expects-accept";

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
    // naming the case. run_cases does no registry I/O, so this Err IS the
    // before-any-push proof (build.rs::run `?`-propagates it before the push).
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
                     no push reached (run_cases does no registry I/O)"
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
              "input":{"hold":{"material":"x","moisture_pct":"abc","moisture_max_pct":"5.00"}},
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
              "input":{"hold":{"material":"resin-A","moisture_pct":"12.00","moisture_max_pct":"5.00"}},
              "expect":{"ok":{"value":{"recommended":"reject"},"match":"subset","port":"reject-branch"}}
            }]}"#,
        )
        .expect("parses");
        let err = run_cases(&bytes, &cases)
            .await
            .expect_err("a wrong port must refuse even when the value matches");
        assert!(err.downcast_ref::<TestGateError>().is_some());
    }
}

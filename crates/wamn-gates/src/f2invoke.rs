//! `f2invoke` — the POC-F2 (wamn-1ab) custom-node INVOCATION gate: the "call it"
//! proof for the zero-import disposition-recommendation node.
//!
//! Where `nodeinvoke` drives the FULL cross-pod machinery (real runner → HTTP
//! hop → serve-node, with credential grants + signed envelopes), F2's node
//! imports NOTHING and reads no credential — so there is no grant to prove at
//! invocation (the grant-derivation proof is the builder + emission side:
//! `f2-build-job` + the golden_deploy tests). This gate therefore does the
//! minimal honest thing: warm-instantiate the REAL compiled node in a REAL
//! [`ServeNode`] host (the same production host `nodeinvoke` uses) and
//! `.invoke()` it in-process with a representative input per disposition outcome
//! plus a malformed one, asserting the node's recommendation and the
//! `InvalidInput` taxonomy arm over the real `wamn:node` ABI.
//!
//! In-cluster it runs from the gates image against `/bench/disposition-node.wasm`
//! (deploy/gates/f2invoke-job.yaml); topology-independent, so local == in-cluster.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use clap::Args;

use wamn_gate_harness::check;
use wamn_host::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_host::plugins::wamn_credentials::WamnCredentials;
use wamn_host::serve_node::{self, ServeNode, ServeNodeAuthn};
use wamn_node_invoke::{
    NodeInvokeRequest, NodeInvokeResponse, WireNodeError, WirePayload, WireRunContext,
};

#[derive(Debug, Args)]
pub struct F2InvokeArgs {
    /// The compiled zero-import disposition-recommendation node
    /// (`components/samples/disposition-node`, built for wasm32-wasip2).
    #[arg(long, default_value = "/bench/disposition-node.wasm")]
    pub node: PathBuf,
}

/// The invocation envelope the runner would POST — a fixed ctx (empty config)
/// with the input under test and NO credential grant (a world node reads none).
fn request(input: &str) -> NodeInvokeRequest {
    NodeInvokeRequest {
        ctx: WireRunContext {
            run_id: "f2".to_string(),
            flow_id: "f2-disposition".to_string(),
            flow_version: 1,
            node_id: "recommend".to_string(),
            attempt: 0,
            idempotency_key: "f2:recommend".to_string(),
            deadline_ms: None,
            traceparent: None,
            tracestate: None,
            config: "{}".to_string(),
        },
        input: WirePayload::Inline(input.to_string()),
        grant: Vec::new(),
    }
}

/// The inline emission payload (parsed) + its port, when the response is a
/// success emission.
fn emission(resp: &NodeInvokeResponse) -> Option<(serde_json::Value, &Option<String>)> {
    match resp {
        NodeInvokeResponse::Ok(em) => match &em.payload {
            WirePayload::Inline(s) => serde_json::from_str(s).ok().map(|v| (v, &em.port)),
        },
        NodeInvokeResponse::Err(_) => None,
    }
}

pub async fn run(args: F2InvokeArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    println!(
        "# wamn-gates f2invoke — POC-F2 zero-import disposition node invocation (5.6, in-proc)"
    );

    let wasm = std::fs::read(&args.node)
        .with_context(|| format!("read disposition node {}", args.node.display()))?;

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);

    // A world node: empty vault (no credentials), no signing key (network-trust,
    // so the direct `.invoke()` is admitted), deny-all egress (it makes none).
    let serve = ServeNode::new(
        &engine,
        &wasm,
        Arc::new(WamnCredentials::empty()),
        serve_node::DEFAULT_NODE_ID,
        "default",
        Arc::from([]),
        ServeNodeAuthn {
            require_signing_key: false,
            max_signature_age_secs: None,
        },
    )
    .await
    .context("warm-instantiate the disposition node (screens the E17 tenant profile)")?;

    let mut ok = true;

    // One representative input per disposition outcome (the catalog enum). Severe
    // moisture -> reject; mild -> use-as-is; in-spec -> accept.
    for (label, input, expected) in [
        (
            "REJECT",
            r#"{"hold":{"material":"resin-A","moisture_pct":"12.00","moisture_max_pct":"5.00"}}"#,
            "reject",
        ),
        (
            "USE-AS-IS",
            r#"{"hold":{"material":"resin-A","moisture_pct":"6.00","moisture_max_pct":"5.00"}}"#,
            "use-as-is",
        ),
        (
            "ACCEPT",
            r#"{"hold":{"material":"resin-A","moisture_pct":"4.00","moisture_max_pct":"5.00"}}"#,
            "accept",
        ),
    ] {
        let resp = serve.invoke(request(input)).await;
        match emission(&resp) {
            Some((v, port)) => {
                let rec = v.get("recommended").and_then(|x| x.as_str());
                let conf = v.get("confidence").and_then(|x| x.as_f64());
                let rationale = v.get("rationale").and_then(|x| x.as_str());
                check(
                    &mut ok,
                    &format!("{label}: recommends {expected}"),
                    rec == Some(expected),
                );
                check(
                    &mut ok,
                    &format!("{label}: emits on the main port (absent)"),
                    port.is_none(),
                );
                check(
                    &mut ok,
                    &format!("{label}: confidence in (0,1]"),
                    conf.is_some_and(|c| c > 0.0 && c <= 1.0),
                );
                check(
                    &mut ok,
                    &format!("{label}: carries a non-empty rationale"),
                    rationale.is_some_and(|r| !r.is_empty()),
                );
                println!("  {label}: recommended={rec:?} confidence={conf:?}");
            }
            None => check(
                &mut ok,
                &format!("{label}: returned a success emission"),
                false,
            ),
        }
    }

    // A malformed hold (a non-decimal moisture) surfaces as InvalidInput over the
    // real WIT boundary — the frozen taxonomy arm the runner never retries.
    let bad = r#"{"hold":{"material":"resin-A","moisture_pct":"not-a-decimal","moisture_max_pct":"5.00"}}"#;
    let resp = serve.invoke(request(bad)).await;
    check(
        &mut ok,
        "INVALID-INPUT: a malformed decimal is InvalidInput at the WIT boundary",
        matches!(
            resp,
            NodeInvokeResponse::Err(WireNodeError::InvalidInput(_))
        ),
    );

    // Zero-import witness: the node ran with no credential grant, and none was
    // left installed (there is nothing to grant a world node).
    check(
        &mut ok,
        "ZERO-IMPORT: no per-invocation grant is left installed (a world node reads none)",
        !serve.invocation_grant_active(),
    );

    ticker.abort();
    println!("\nf2invoke complete — overall PASS: {ok}");
    if !ok {
        bail!("POC-F2 f2invoke gate failed");
    }
    Ok(())
}

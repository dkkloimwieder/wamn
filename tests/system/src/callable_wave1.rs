//! Composed Wave-1 proof for callable flows F0, F1, and F3.

use std::path::Path;

use anyhow::{Context as _, ensure};
use clap::Args;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use wamn_flow::canonical_json_sha256;
use wamn_run_state::attempt::{AttemptDispatchResult, AttemptStartResult};
use wamn_run_state::context;
use wamn_run_state::transitions::{
    begin_attempt_sql, mark_attempt_dispatched_sql, node_context_checkpoint_sql,
};

use crate::{callable_f0, callable_f1, callable_f3};

const CATALOG_JSON: &str = include_str!("../../../deploy/poc/poc-material-receiving.catalog.json");
const CONFIG_JSON: &str = include_str!("../../../deploy/poc/poc-material-receiving.config.json");
const F0_FLOW_JSON: &str = include_str!("../../../deploy/poc/f0-flow.json");
const F0_EXPOSURE_JSON: &str = include_str!("../../../deploy/poc/f0-http-attachment.json");
const F1_FLOW_JSON: &str = include_str!("../../../deploy/poc/f1-flow.json");
const F1_EXPOSURE_JSON: &str = include_str!("../../../deploy/poc/f1-http-attachment.json");
const F3_FLOW_JSON: &str = include_str!("../../../deploy/poc/f3-flow.json");
const F3_EXPOSURE_JSON: &str = include_str!("../../../deploy/poc/f3-cron-attachment.json");

#[derive(Debug, Args)]
pub struct CallableWave1IdentityArgs {
    /// Git commit from which the exact gates image was built.
    #[arg(long)]
    pub source_identity: String,

    /// Exact locally loaded image selected by the gate Job.
    #[arg(long)]
    pub image_identity: String,

    /// Commit-bound Kubernetes deployment identity.
    #[arg(long)]
    pub deployment_identity: String,

    /// Exact normalize-receipt component baked into the gates image.
    #[arg(long, default_value = "/bench/normalize-receipt.wasm")]
    pub normalize_receipt: std::path::PathBuf,

    /// Exact evaluate-specs component baked into the gates image.
    #[arg(long, default_value = "/bench/evaluate-specs.wasm")]
    pub evaluate_specs: std::path::PathBuf,
}

pub fn run(args: CallableWave1IdentityArgs) -> anyhow::Result<String> {
    validate_identity(&args)?;
    prove_contracts()?;

    let receipt = identity_receipt(&args)?;
    println!("callable-flow-wave1 PASS: T0/T-CTX/T-NR/T1/T3; identity-receipt={receipt}");
    Ok(receipt)
}

/// Runs the Wave-1 contract and recovery proofs without issuing a Wave-1 receipt.
pub fn prove_contracts() -> anyhow::Result<()> {
    callable_f0::run(callable_f0::CallableF0Args {})?;
    callable_f1::run(callable_f1::CallableF1Args {})?;
    callable_f3::run(callable_f3::CallableF3Args {})?;
    prove_runtime_contracts()
}

fn validate_identity(args: &CallableWave1IdentityArgs) -> anyhow::Result<()> {
    ensure!(
        args.source_identity.len() == 40
            && args
                .source_identity
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
        "source identity must be a lowercase 40-byte Git commit"
    );
    ensure!(
        args.image_identity == format!("wamn-gates:cf-wave1-{}", args.source_identity),
        "image identity is not bound to the source commit"
    );
    ensure!(
        args.deployment_identity == format!("callable-flow-wave1@{}", args.source_identity),
        "deployment identity is not bound to the source commit"
    );
    Ok(())
}

fn identity_receipt(args: &CallableWave1IdentityArgs) -> anyhow::Result<String> {
    let release_inputs = json!({
        "f0": {
            "flow": digest(F0_FLOW_JSON.as_bytes()),
            "exposure": digest(F0_EXPOSURE_JSON.as_bytes()),
        },
        "f1": {
            "flow": digest(F1_FLOW_JSON.as_bytes()),
            "exposure": digest(F1_EXPOSURE_JSON.as_bytes()),
        },
        "f3": {
            "flow": digest(F3_FLOW_JSON.as_bytes()),
            "exposure": digest(F3_EXPOSURE_JSON.as_bytes()),
        },
    });
    let receipt = json!({
        "source": args.source_identity,
        "image": args.image_identity,
        "components": {
            "normalize-receipt": file_digest(&args.normalize_receipt)?,
            "evaluate-specs": file_digest(&args.evaluate_specs)?,
        },
        "config": digest(CONFIG_JSON.as_bytes()),
        "schema": digest(CATALOG_JSON.as_bytes()),
        "release-inputs": release_inputs,
        "deployment": args.deployment_identity,
    });
    Ok(canonical_json_sha256(&receipt))
}

fn file_digest(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    ensure!(!bytes.is_empty(), "{} is empty", path.display());
    Ok(digest(&bytes))
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn prove_runtime_contracts() -> anyhow::Result<()> {
    let context = context::replace(None, json!({"cutoff": "2026-07-26T12:00:00Z"}))?;
    let context = context::replace(Some(context), json!({"hold": {"id": 7}}))?;
    ensure!(
        context::read(Some(&context))? == json!({"hold": {"id": 7}}),
        "T-CTX replacement implicitly merged prior context"
    );

    let checkpoint = node_context_checkpoint_sql();
    ensure!(
        checkpoint.contains("$16::text::jsonb")
            && checkpoint.contains("(SELECT count(*) FROM recorded) = 1"),
        "T-CTX output/context checkpoint lost atomicity"
    );

    let begin = begin_attempt_sql();
    ensure!(
        begin.contains("n.recovery_class = 'never-replay' THEN 'effect-uncertain'")
            && begin.contains("n.recovery_class IN ('replay', 'idempotent-with-key')")
            && !AttemptStartResult::EffectUncertain.permits_dispatch()
            && !AttemptStartResult::MissingAttemptKey.permits_dispatch(),
        "T-NR recovery classification drift"
    );
    let dispatch = mark_attempt_dispatched_sql();
    ensure!(
        dispatch.contains("SET attempt_dispatched_at = now()")
            && AttemptDispatchResult::Marked.permits_dispatch()
            && !AttemptDispatchResult::AlreadyDispatched.permits_dispatch(),
        "T-NR crash-before-send dispatch fence drift"
    );
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    fn fixture(root: &Path) -> CallableWave1IdentityArgs {
        CallableWave1IdentityArgs {
            source_identity: "a".repeat(40),
            image_identity: format!("wamn-gates:cf-wave1-{}", "a".repeat(40)),
            deployment_identity: format!("callable-flow-wave1@{}", "a".repeat(40)),
            normalize_receipt: root.join("normalize-receipt.wasm"),
            evaluate_specs: root.join("evaluate-specs.wasm"),
        }
    }

    #[test]
    fn identity_receipt_binds_every_wave1_input() {
        let root = std::env::temp_dir().join(format!("wamn-callable-wave1-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("normalize-receipt.wasm"), b"normalize").unwrap();
        std::fs::write(root.join("evaluate-specs.wasm"), b"evaluate").unwrap();
        let args = fixture(&root);
        validate_identity(&args).unwrap();
        let first = identity_receipt(&args).unwrap();

        std::fs::write(root.join("evaluate-specs.wasm"), b"changed").unwrap();
        let second = identity_receipt(&args).unwrap();
        assert_ne!(first, second);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_image_and_deployment_drift_are_refused() {
        let root = Path::new(".");
        let mut args = fixture(root);
        args.source_identity = "not-a-commit".to_string();
        assert!(validate_identity(&args).is_err());

        let mut args = fixture(root);
        args.image_identity = "wamn-gates:cf-wave1-stale".to_string();
        assert!(validate_identity(&args).is_err());

        let mut args = fixture(root);
        args.deployment_identity = "callable-flow-wave1@stale".to_string();
        assert!(validate_identity(&args).is_err());
    }

    #[test]
    fn runtime_contracts_cover_t_ctx_and_t_nr() {
        prove_runtime_contracts().unwrap();
    }

    #[test]
    fn exact_image_job_routes_to_the_composed_proof() {
        let router = include_str!("../../orchestrator/src/main.rs");
        assert!(router.contains("CallableWave1(callable_wave1::CallableWave1Args)"));
        assert!(router.contains("Command::CallableWave1(args) => callable_wave1::run(args).await"));

        let job = include_str!("../../../deploy/gates/callable-flow-wave1-job.yaml");
        assert!(job.contains("name: callable-flow-wave1"));
        assert!(job.contains("image: wamn-gates:cf-wave1-ISSUE"));
        assert!(job.contains("imagePullPolicy: Never"));
        assert!(job.contains("\"--source-identity\", \"ISSUE\""));
        assert!(job.contains("\"--image-identity\", \"wamn-gates:cf-wave1-ISSUE\""));
        assert!(job.contains("\"--deployment-identity\", \"callable-flow-wave1@ISSUE\""));
        assert!(!job.contains("wamn-gates:dev"));
    }
}

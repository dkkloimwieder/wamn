//! Composed Wave-2 proof for callable flows F0 through F4.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use clap::Args;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use wamn_flow::canonical_json_sha256;

use crate::{callable_f2, callable_f4, callable_wave1};

const CATALOG_JSON: &str = include_str!("../../../deploy/poc/poc-material-receiving.catalog.json");
const CONFIG_JSON: &str = include_str!("../../../deploy/poc/poc-material-receiving.config.json");
const F0_FLOW_JSON: &str = include_str!("../../../deploy/poc/f0-flow.json");
const F0_EXPOSURE_JSON: &str = include_str!("../../../deploy/poc/f0-http-attachment.json");
const F1_FLOW_JSON: &str = include_str!("../../../deploy/poc/f1-flow.json");
const F1_EXPOSURE_JSON: &str = include_str!("../../../deploy/poc/f1-http-attachment.json");
const F2_FLOW_JSON: &str = include_str!("../../../deploy/poc/f2-flow.json");
const F2_EXPOSURE_JSON: &str = include_str!("../../../deploy/poc/f2-internal-attachment.json");
const F3_FLOW_JSON: &str = include_str!("../../../deploy/poc/f3-flow.json");
const F3_EXPOSURE_JSON: &str = include_str!("../../../deploy/poc/f3-cron-attachment.json");
const F4_FLOW_JSON: &str = include_str!("../../../deploy/poc/f4-flow.json");
const F4_REGISTRATION_JSON: &str = include_str!("../../../deploy/poc/f4-event-registration.json");

#[derive(Debug, Args)]
pub struct CallableWave2IdentityArgs {
    /// Git commit from which the exact gates image was built.
    #[arg(long)]
    pub source_identity: String,

    /// Exact image tag selected by the gate Job.
    #[arg(long)]
    pub image_identity: String,

    /// Exact image ID observed by Kubernetes after loading the tag into kind.
    #[arg(long)]
    pub image_id: String,

    /// Commit-bound Kubernetes deployment identity.
    #[arg(long)]
    pub deployment_identity: String,

    /// Exact flowrunner component baked into the gates image.
    #[arg(long, default_value = "/bench/flowrunner.wasm")]
    pub flowrunner: PathBuf,

    /// Exact normalize-receipt component baked into the gates image.
    #[arg(long, default_value = "/bench/normalize-receipt.wasm")]
    pub normalize_receipt: PathBuf,

    /// Exact evaluate-specs component baked into the gates image.
    #[arg(long, default_value = "/bench/evaluate-specs.wasm")]
    pub evaluate_specs: PathBuf,

    /// Exact disposition-node component baked into the gates image.
    #[arg(long, default_value = "/bench/disposition-node.wasm")]
    pub disposition_node: PathBuf,
}

pub fn run(args: CallableWave2IdentityArgs) -> anyhow::Result<String> {
    println!("claimed-image-id={}", args.image_id);
    validate_identity(&args)?;
    callable_wave1::prove_contracts()?;
    callable_f2::run(callable_f2::CallableF2Args {
        component: args.disposition_node.clone(),
    })?;
    callable_f4::run(callable_f4::CallableF4Args {})?;
    prove_t5_hooks()?;

    let receipt = identity_receipt(&args)?;
    println!(
        "callable-flow-wave2 PASS: T0/T2/T4 plus T0/T-CTX/T-NR/T1/T3 regression; \
         T5 hooks recorded without budgets; identity-receipt={receipt}"
    );
    Ok(receipt)
}

fn validate_identity(args: &CallableWave2IdentityArgs) -> anyhow::Result<()> {
    ensure!(
        args.source_identity.len() == 40
            && args
                .source_identity
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
        "source identity must be a lowercase 40-byte Git commit"
    );
    ensure!(
        args.image_identity == format!("wamn-gates:cf-wave2-{}", args.source_identity),
        "image tag is not bound to the source commit"
    );
    ensure!(
        args.image_id.starts_with("sha256:")
            && args.image_id.len() == 71
            && args.image_id[7..]
                .bytes()
                .all(|byte| { byte.is_ascii_digit() || matches!(byte, b'a'..=b'f') }),
        "image ID must be a lowercase sha256 digest"
    );
    ensure!(
        args.deployment_identity == format!("callable-flow-wave2@{}", args.source_identity),
        "deployment identity is not bound to the source commit"
    );
    Ok(())
}

fn identity_receipt(args: &CallableWave2IdentityArgs) -> anyhow::Result<String> {
    Ok(canonical_json_sha256(&receipt_document(args)?))
}

fn receipt_document(args: &CallableWave2IdentityArgs) -> anyhow::Result<Value> {
    Ok(json!({
        "source": args.source_identity,
        "image": {
            "tag": args.image_identity,
            "id": args.image_id,
        },
        "components": {
            "flowrunner": file_digest(&args.flowrunner)?,
            "normalize-receipt": file_digest(&args.normalize_receipt)?,
            "evaluate-specs": file_digest(&args.evaluate_specs)?,
            "disposition-node": file_digest(&args.disposition_node)?,
        },
        "config": digest(CONFIG_JSON.as_bytes()),
        "schema": digest(CATALOG_JSON.as_bytes()),
        "release-inputs": {
            "f0": {
                "graph": digest(F0_FLOW_JSON.as_bytes()),
                "http-attachment": digest(F0_EXPOSURE_JSON.as_bytes()),
            },
            "f1": {
                "graph": digest(F1_FLOW_JSON.as_bytes()),
                "http-attachment": digest(F1_EXPOSURE_JSON.as_bytes()),
            },
            "f2": {
                "graph": digest(F2_FLOW_JSON.as_bytes()),
                "internal-attachment": digest(F2_EXPOSURE_JSON.as_bytes()),
            },
            "f3": {
                "graph": digest(F3_FLOW_JSON.as_bytes()),
                "cron-attachment": digest(F3_EXPOSURE_JSON.as_bytes()),
            },
            "f4": {
                "graph": digest(F4_FLOW_JSON.as_bytes()),
                "event-registration": digest(F4_REGISTRATION_JSON.as_bytes()),
            },
        },
        "release": {
            "tenant-id": "poc",
            "catalog-id": "callable",
            "catalog-version": 6,
            "flows": ["echo", "receipt-received", "disposition-recommendation",
                      "escalate-stale-holds", "disposition-recorded"],
        },
        "deployment": args.deployment_identity,
        "t5-hooks": t5_hooks(),
    }))
}

fn file_digest(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    ensure!(!bytes.is_empty(), "{} is empty", path.display());
    Ok(digest(&bytes))
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn t5_hooks() -> Value {
    json!({
        "f0-pure-two-commit": ["commits", "sql-statements", "rows", "wal-bytes", "latency"],
        "f1-effectful-request": ["commits", "sql-statements", "rows", "wal-bytes", "latency"],
        "f3-queued-cron": ["commits", "sql-statements", "rows", "wal-bytes", "recovery-latency"],
        "f4-f2-idempotent-child": [
            "commits", "sql-statements", "rows", "wal-bytes", "latency", "recovery-latency"
        ],
        "budgets": null,
    })
}

fn prove_t5_hooks() -> anyhow::Result<()> {
    let hooks = t5_hooks();
    ensure!(
        hooks.as_object().is_some_and(|values| values.len() == 5)
            && hooks["f0-pure-two-commit"].as_array().is_some()
            && hooks["f1-effectful-request"].as_array().is_some()
            && hooks["f3-queued-cron"].as_array().is_some()
            && hooks["f4-f2-idempotent-child"].as_array().is_some()
            && hooks["budgets"].is_null(),
        "T5 hooks must cover all four shapes without claiming Phase-6 budgets"
    );
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn fixture(root: &Path) -> CallableWave2IdentityArgs {
        let commit = "a".repeat(40);
        CallableWave2IdentityArgs {
            source_identity: commit.clone(),
            image_identity: format!("wamn-gates:cf-wave2-{commit}"),
            image_id: format!("sha256:{}", "b".repeat(64)),
            deployment_identity: format!("callable-flow-wave2@{commit}"),
            flowrunner: root.join("flowrunner.wasm"),
            normalize_receipt: root.join("normalize-receipt.wasm"),
            evaluate_specs: root.join("evaluate-specs.wasm"),
            disposition_node: root.join("disposition-node.wasm"),
        }
    }

    fn component_fixture() -> (PathBuf, CallableWave2IdentityArgs) {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "wamn-callable-wave2-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for name in [
            "flowrunner.wasm",
            "normalize-receipt.wasm",
            "evaluate-specs.wasm",
            "disposition-node.wasm",
        ] {
            std::fs::write(root.join(name), name.as_bytes()).unwrap();
        }
        let args = fixture(&root);
        (root, args)
    }

    #[test]
    fn receipt_binds_all_five_releases_components_and_t5_hooks() {
        let (root, args) = component_fixture();
        let document = receipt_document(&args).unwrap();
        assert_eq!(document["release-inputs"].as_object().unwrap().len(), 5);
        assert_eq!(document["components"].as_object().unwrap().len(), 4);
        assert_eq!(document["release"]["flows"].as_array().unwrap().len(), 5);
        assert!(document["t5-hooks"]["budgets"].is_null());

        let first = identity_receipt(&args).unwrap();
        std::fs::write(&args.disposition_node, b"changed").unwrap();
        assert_ne!(first, identity_receipt(&args).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mixed_source_image_and_deployment_identities_are_refused() {
        let (root, mut args) = component_fixture();
        let source = args.source_identity.clone();
        let image = args.image_identity.clone();
        let image_id = args.image_id.clone();

        args.source_identity = "not-a-commit".to_string();
        assert!(validate_identity(&args).is_err());
        args.source_identity = source;

        args.image_identity = "wamn-gates:cf-wave2-stale".to_string();
        assert!(validate_identity(&args).is_err());
        args.image_identity = image;

        args.image_id = "sha256:not-a-digest".to_string();
        assert!(validate_identity(&args).is_err());
        args.image_id = image_id;

        args.deployment_identity = "callable-flow-wave2@stale".to_string();
        assert!(validate_identity(&args).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn t5_hooks_are_recorded_without_phase6_budgets() {
        prove_t5_hooks().unwrap();
    }

    #[test]
    fn archived_job_has_no_orchestrator_route() {
        let router = include_str!("../../orchestrator/src/main.rs");
        assert!(!router.contains("CallableWave2(callable_wave2::CallableWave2Args)"));
        assert!(
            !router.contains("Command::CallableWave2(args) => callable_wave2::run(args).await")
        );
    }
}

//! Strict schema and invariant checks for checked-in gate-mutation receipts.

use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const RECEIPT_DIRECTORY: &str = "architecture/receipts/mutations";
const DURABLE_CAMPAIGN: &str = "durable-invocation-recovery";
const DURABLE_BEAD: &str = "wamn-2jdm.5.1";
const DURABLE_RUNNER: &str = "tools/gate-mutants/durable-invocation-recovery.sh";
const DURABLE_SOURCE_COMMIT: &str = "cf9d5ffebc885629bf2f7c45a2310f6c55245f60";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u32,
    campaign: String,
    bead: String,
    source: SourceIdentity,
    profile: String,
    green_runs: Vec<GreenRun>,
    mutants: Vec<MutantRun>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceIdentity {
    git_commit: String,
    runner_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GreenRun {
    gate: String,
    command: Vec<String>,
    exit_code: i32,
    log_sha256: String,
    status: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MutantRun {
    id: String,
    gate: String,
    target: String,
    command: Vec<String>,
    baseline_sha256: String,
    mutant_sha256: String,
    restored_sha256: String,
    exit_code: i32,
    log_sha256: String,
    status: String,
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn validate_sha256(value: &str, field: &str) -> Result<(), String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{field} is not a SHA-256 digest"));
    }
    Ok(())
}

fn validate_command(command: &[String], field: &str) -> Result<(), String> {
    if command.len() < 2 || command[0] != "cargo" || command[1] != "test" {
        return Err(format!("{field} must be a fixed cargo test argv"));
    }
    if command.iter().any(|argument| argument.trim().is_empty()) {
        return Err(format!("{field} contains an empty argument"));
    }
    Ok(())
}

fn validate_receipt(receipt: &Receipt) -> Result<(), String> {
    if receipt.schema_version != 1 {
        return Err(format!(
            "unsupported mutation receipt schema {}",
            receipt.schema_version
        ));
    }
    if receipt.campaign.trim().is_empty() || receipt.bead.trim().is_empty() {
        return Err("receipt campaign and bead must be named".to_string());
    }
    if receipt.profile != "debug" {
        return Err("mutation receipts must use the debug profile".to_string());
    }
    if receipt.source.git_commit.len() != 40
        || !receipt
            .source
            .git_commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("source.git_commit must be a full commit id".to_string());
    }
    validate_sha256(&receipt.source.runner_sha256, "source.runner_sha256")?;
    if receipt.green_runs.is_empty() || receipt.mutants.is_empty() {
        return Err("receipt requires green and red evidence".to_string());
    }

    let mut green_gates = BTreeSet::new();
    for run in &receipt.green_runs {
        if !green_gates.insert(run.gate.as_str()) {
            return Err(format!("duplicate green gate {}", run.gate));
        }
        validate_command(&run.command, "green command")?;
        validate_sha256(&run.log_sha256, "green log_sha256")?;
        if run.exit_code != 0 || run.status != "passed" {
            return Err(format!("green gate {} did not pass", run.gate));
        }
    }

    let mut mutant_ids = BTreeSet::new();
    for mutant in &receipt.mutants {
        if !mutant_ids.insert(mutant.id.as_str()) {
            return Err(format!("duplicate mutant {}", mutant.id));
        }
        if mutant.gate.trim().is_empty() || mutant.target.trim().is_empty() {
            return Err(format!("mutant {} has incomplete identity", mutant.id));
        }
        validate_command(&mutant.command, "mutant command")?;
        validate_sha256(&mutant.baseline_sha256, "mutant baseline_sha256")?;
        validate_sha256(&mutant.mutant_sha256, "mutant mutant_sha256")?;
        validate_sha256(&mutant.restored_sha256, "mutant restored_sha256")?;
        validate_sha256(&mutant.log_sha256, "mutant log_sha256")?;
        if mutant.baseline_sha256 == mutant.mutant_sha256 {
            return Err(format!("mutant {} did not change its target", mutant.id));
        }
        if mutant.baseline_sha256 != mutant.restored_sha256 {
            return Err(format!("mutant {} did not restore byte-exactly", mutant.id));
        }
        if mutant.exit_code == 0 || mutant.status != "killed" {
            return Err(format!("mutant {} was not killed", mutant.id));
        }
        if !green_gates.contains(mutant.gate.as_str()) {
            return Err(format!(
                "mutant {} lacks a matching green gate run",
                mutant.id
            ));
        }
    }
    Ok(())
}

fn receipt_json() -> String {
    format!(
        r#"{{
          "schema_version": 1,
          "campaign": "{DURABLE_CAMPAIGN}",
          "bead": "{DURABLE_BEAD}",
          "source": {{
            "git_commit": "1111111111111111111111111111111111111111",
            "runner_sha256": "{digest}"
          }},
          "profile": "debug",
          "green_runs": [{{
            "gate": "gate-a",
            "command": ["cargo", "test", "--locked", "-p", "proof", "gate-a"],
            "exit_code": 0,
            "log_sha256": "{digest}",
            "status": "passed"
          }}],
          "mutants": [{{
            "id": "mutant-a",
            "gate": "gate-a",
            "target": "src/lib.rs",
            "command": ["cargo", "test", "--locked", "-p", "proof", "gate-a"],
            "baseline_sha256": "{digest}",
            "mutant_sha256": "{mutant_digest}",
            "restored_sha256": "{digest}",
            "exit_code": 101,
            "log_sha256": "{digest}",
            "status": "killed"
          }}]
        }}"#,
        digest = "2".repeat(64),
        mutant_digest = "3".repeat(64),
    )
}

fn parse_receipt(json: &str) -> Result<Receipt, String> {
    serde_json::from_str(json).map_err(|error| format!("invalid receipt JSON: {error}"))
}

#[test]
fn strict_receipt_schema_accepts_green_red_and_byte_exact_restore() {
    let receipt = parse_receipt(&receipt_json()).expect("valid receipt fixture");
    validate_receipt(&receipt).expect("valid receipt invariants");
}

#[test]
fn strict_receipt_schema_rejects_unknown_fields_and_non_debug_runs() {
    let unknown = receipt_json().replacen(
        "\"profile\": \"debug\"",
        "\"profile\": \"debug\", \"mutable_note\": true",
        1,
    );
    assert!(parse_receipt(&unknown).is_err());

    let release = receipt_json().replacen("\"profile\": \"debug\"", "\"profile\": \"release\"", 1);
    let receipt = parse_receipt(&release).expect("release is structurally valid JSON");
    assert_eq!(
        validate_receipt(&receipt),
        Err("mutation receipts must use the debug profile".to_string())
    );
}

#[test]
fn strict_receipt_schema_rejects_survivors_and_non_restoration() {
    let survived = receipt_json()
        .replacen("\"exit_code\": 101", "\"exit_code\": 0", 1)
        .replacen("\"status\": \"killed\"", "\"status\": \"survived\"", 1);
    let receipt = parse_receipt(&survived).expect("survivor fixture parses");
    assert!(
        validate_receipt(&receipt)
            .expect_err("survivor must fail")
            .contains("was not killed")
    );

    let non_restored = receipt_json().replacen(
        &format!("\"restored_sha256\": \"{}\"", "2".repeat(64)),
        &format!("\"restored_sha256\": \"{}\"", "4".repeat(64)),
        1,
    );
    let receipt = parse_receipt(&non_restored).expect("non-restored fixture parses");
    assert!(
        validate_receipt(&receipt)
            .expect_err("non-restoration must fail")
            .contains("did not restore byte-exactly")
    );
}

#[test]
fn checked_in_mutation_receipts_conform_when_present() {
    let directory = repository_root().join(RECEIPT_DIRECTORY);
    if !directory.exists() {
        return;
    }
    for entry in fs::read_dir(&directory).expect("mutation receipt directory is readable") {
        let path = entry.expect("mutation receipt entry is readable").path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let json = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        let receipt =
            parse_receipt(&json).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        validate_receipt(&receipt).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        if receipt.campaign == DURABLE_CAMPAIGN {
            assert_eq!(receipt.bead, DURABLE_BEAD);
            assert_eq!(receipt.source.git_commit, DURABLE_SOURCE_COMMIT);
            let runner = fs::read(repository_root().join(DURABLE_RUNNER))
                .expect("durable mutation runner is readable");
            assert_eq!(
                receipt.source.runner_sha256,
                hex::encode(Sha256::digest(runner)),
                "durable receipt must identify the checked-in runner bytes"
            );
        }
    }
}

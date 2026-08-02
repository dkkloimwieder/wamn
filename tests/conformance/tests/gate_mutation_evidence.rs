//! Strict schema and invariant checks for checked-in gate-mutation evidence.

use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const EVIDENCE_DIRECTORY: &str = "architecture/evidence/mutations";
const DURABLE_CAMPAIGN: &str = "durable-invocation-recovery";
const DURABLE_BEAD: &str = "wamn-2jdm.5.1";
const DURABLE_RUNNER: &str = "tools/gate-mutants/durable-invocation-recovery.sh";
const DURABLE_SOURCE_COMMIT: &str = "cf9d5ffebc885629bf2f7c45a2310f6c55245f60";
const QUEUE_CAMPAIGN: &str = "queue-runner";
const QUEUE_BEAD: &str = "wamn-2jdm.5.2";
const QUEUE_RUNNER: &str = "tools/gate-mutants/queue-runner.sh";
const QUEUE_SOURCE_COMMIT: &str = "c51ca79516a8195b83db1572ae1d60f570bebef2";
const SCENARIO_CAMPAIGN: &str = "scenario-replay-impact";
const SCENARIO_BEAD: &str = "wamn-2jdm.5.3";
const SCENARIO_RUNNER: &str = "tools/gate-mutants/scenario-replay-impact.sh";
const SCENARIO_SOURCE_COMMIT: &str = "3b866e82725b84eea40f513d81838b6c7fcbfadf";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceRecord {
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

fn validate_evidence(evidence: &EvidenceRecord) -> Result<(), String> {
    if evidence.schema_version != 1 {
        return Err(format!(
            "unsupported mutation evidence schema {}",
            evidence.schema_version
        ));
    }
    if evidence.campaign.trim().is_empty() || evidence.bead.trim().is_empty() {
        return Err("evidence campaign and bead must be named".to_string());
    }
    if evidence.profile != "debug" {
        return Err("mutation evidence must use the debug profile".to_string());
    }
    if evidence.source.git_commit.len() != 40
        || !evidence
            .source
            .git_commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("source.git_commit must be a full commit id".to_string());
    }
    validate_sha256(&evidence.source.runner_sha256, "source.runner_sha256")?;
    if evidence.green_runs.is_empty() || evidence.mutants.is_empty() {
        return Err("evidence requires green and red evidence".to_string());
    }

    let mut green_gates = BTreeSet::new();
    for run in &evidence.green_runs {
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
    for mutant in &evidence.mutants {
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

fn evidence_json() -> String {
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

fn parse_evidence(json: &str) -> Result<EvidenceRecord, String> {
    serde_json::from_str(json).map_err(|error| format!("invalid evidence JSON: {error}"))
}

#[test]
fn strict_evidence_schema_accepts_green_red_and_byte_exact_restore() {
    let evidence = parse_evidence(&evidence_json()).expect("valid evidence fixture");
    validate_evidence(&evidence).expect("valid evidence invariants");
}

#[test]
fn strict_evidence_schema_rejects_unknown_fields_and_non_debug_runs() {
    let unknown = evidence_json().replacen(
        "\"profile\": \"debug\"",
        "\"profile\": \"debug\", \"mutable_note\": true",
        1,
    );
    assert!(parse_evidence(&unknown).is_err());

    let release = evidence_json().replacen("\"profile\": \"debug\"", "\"profile\": \"release\"", 1);
    let evidence = parse_evidence(&release).expect("release is structurally valid JSON");
    assert_eq!(
        validate_evidence(&evidence),
        Err("mutation evidence must use the debug profile".to_string())
    );
}

#[test]
fn strict_evidence_schema_rejects_survivors_and_non_restoration() {
    let survived = evidence_json()
        .replacen("\"exit_code\": 101", "\"exit_code\": 0", 1)
        .replacen("\"status\": \"killed\"", "\"status\": \"survived\"", 1);
    let evidence = parse_evidence(&survived).expect("survivor fixture parses");
    assert!(
        validate_evidence(&evidence)
            .expect_err("survivor must fail")
            .contains("was not killed")
    );

    let non_restored = evidence_json().replacen(
        &format!("\"restored_sha256\": \"{}\"", "2".repeat(64)),
        &format!("\"restored_sha256\": \"{}\"", "4".repeat(64)),
        1,
    );
    let evidence = parse_evidence(&non_restored).expect("non-restored fixture parses");
    assert!(
        validate_evidence(&evidence)
            .expect_err("non-restoration must fail")
            .contains("did not restore byte-exactly")
    );
}

#[test]
fn checked_in_mutation_evidence_conforms_when_present() {
    let directory = repository_root().join(EVIDENCE_DIRECTORY);
    if !directory.exists() {
        return;
    }
    for entry in fs::read_dir(&directory).expect("mutation evidence directory is readable") {
        let path = entry.expect("mutation evidence entry is readable").path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let json = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        let evidence =
            parse_evidence(&json).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        validate_evidence(&evidence).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        if evidence.campaign == DURABLE_CAMPAIGN {
            assert_eq!(evidence.bead, DURABLE_BEAD);
            assert_eq!(evidence.source.git_commit, DURABLE_SOURCE_COMMIT);
            let runner = fs::read(repository_root().join(DURABLE_RUNNER))
                .expect("durable mutation runner is readable");
            assert_eq!(
                evidence.source.runner_sha256,
                hex::encode(Sha256::digest(runner)),
                "durable evidence must identify the checked-in runner bytes"
            );
        }
        if evidence.campaign == QUEUE_CAMPAIGN {
            assert_eq!(evidence.bead, QUEUE_BEAD);
            assert_eq!(evidence.source.git_commit, QUEUE_SOURCE_COMMIT);
            let runner = fs::read(repository_root().join(QUEUE_RUNNER))
                .expect("queue mutation runner is readable");
            assert_eq!(
                evidence.source.runner_sha256,
                hex::encode(Sha256::digest(runner)),
                "queue evidence must identify the checked-in runner bytes"
            );
        }
        if evidence.campaign == SCENARIO_CAMPAIGN {
            assert_eq!(evidence.bead, SCENARIO_BEAD);
            assert_eq!(evidence.source.git_commit, SCENARIO_SOURCE_COMMIT);
            let runner = fs::read(repository_root().join(SCENARIO_RUNNER))
                .expect("scenario mutation runner is readable");
            assert_eq!(
                evidence.source.runner_sha256,
                hex::encode(Sha256::digest(runner)),
                "scenario evidence must identify the checked-in runner bytes"
            );
        }
    }
}

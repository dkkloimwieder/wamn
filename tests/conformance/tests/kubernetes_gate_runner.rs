//! Machine-verdict protocol conformance over a deterministic fake `kubectl`.

use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use wamn_proof_conformance::kubernetes_gate_receipt::{Expectation, Verdict, parse_receipt};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

const RUNNER: &str = "tools/kubernetes-gate-run";
const EXPECTED_IMAGE: &str = "wamn-gates:dev";

const FAKE_KUBECTL: &str = r#"#!/usr/bin/env bash
set -uo pipefail
printf '%s\n' "$*" >>"$FAKE_CALLS"
shift 2 # -n <namespace>
command=$1
shift
case "$command" in
  get)
    kind=$1
    shift
    if [[ "$kind" == job ]]; then
      name=$1
      if [[ ! -f "$FAKE_APPLIED" ]]; then
        printf '{"metadata":{"uid":"old-%s","creationTimestamp":"2000-01-01T00:00:00Z"}}\n' "$name"
        exit 0
      fi
      condition=Complete
      if [[ "$name" == *refusal* ]]; then condition=Failed; fi
      uid="new-$name"
      created=9998-01-01T00:00:00Z
      transition=9998-01-01T00:00:01Z
      if [[ "$FAKE_SCENARIO" == stale ]]; then
        uid="old-$name"
        created=2000-01-01T00:00:00Z
        transition=2000-01-01T00:00:01Z
      elif [[ "$FAKE_SCENARIO" == negative-pass && "$name" == *refusal* ]]; then
        condition=Complete
      fi
      printf '{"metadata":{"uid":"%s","creationTimestamp":"%s"},"status":{"conditions":[{"type":"%s","status":"True","lastTransitionTime":"%s"}]}}\n' \
        "$uid" "$created" "$condition" "$transition"
    elif [[ "$kind" == pods ]]; then
      selector=$2
      name=${selector#job-name=}
      phase=Succeeded
      exit_code=0
      image="$FAKE_EXPECTED_IMAGE"
      image_id="docker-pullable://wamn@sha256:abc"
      started='"9998-01-01T00:00:00Z"'
      init='[{"name":"ready","state":{"terminated":{"exitCode":0,"finishedAt":"9998-01-01T00:00:00Z"}}}]'
      if [[ "$name" == *refusal* ]]; then phase=Failed; exit_code=1; fi
      case "$FAKE_SCENARIO:$name" in
        never-start:*|multi-failure:second)
          started=null
          ;;
        init-failure:*)
          phase=Failed
          init='[{"name":"ready","state":{"terminated":{"exitCode":1,"finishedAt":"9998-01-01T00:00:00Z"}}}]'
          ;;
        wrong-image:*) image=wrong:latest ;;
        negative-pass:*refusal*) phase=Succeeded; exit_code=0 ;;
        negative-different:*refusal*) exit_code=2 ;;
      esac
      printf '{"items":[{"metadata":{"name":"%s-pod","uid":"pod-%s","creationTimestamp":"9998-01-01T00:00:00Z"},"spec":{"initContainers":[{"name":"ready"}],"containers":[{"name":"%s","image":"%s"}]},"status":{"startTime":%s,"phase":"%s","initContainerStatuses":%s,"containerStatuses":[{"name":"%s","imageID":"%s","state":{"terminated":{"exitCode":%s,"finishedAt":"9998-01-01T00:00:02Z"}}}]}}]}\n' \
        "$name" "$name" "$name" "$image" "$started" "$phase" "$init" "$name" "$image_id" "$exit_code"
    fi
    ;;
  delete) exit 0 ;;
  apply) : >"$FAKE_APPLIED" ;;
  wait)
    resource=$2
    name=${resource#job/}
    if [[ "$FAKE_SCENARIO" == timeout ]]; then exit 1; fi
    if [[ "$FAKE_SCENARIO" == multi-failure && "$name" == second ]]; then exit 1; fi
    ;;
  logs)
    resource=$1
    name=${resource#job/}
    if [[ "$FAKE_SCENARIO" == missing-logs ]]; then exit 1; fi
    if [[ "$name" == *refusal* ]]; then
      printf 'custom-node test gate (11.5): 1 case(s) FAILED against the built artifact\n'
    else
      printf 'overall PASS: true\n'
    fi
    ;;
  *) exit 64 ;;
esac
"#;

const SNAPSHOT_PROBE: &str = r#"#!/usr/bin/env bash
set -uo pipefail
printf '<%s>\n' "$@" >>"$FAKE_PROBE_CALLS"
count=0
if [[ -f "$FAKE_PROBE_COUNT" ]]; then count=$(<"$FAKE_PROBE_COUNT"); fi
count=$((count + 1))
printf '%s' "$count" >"$FAKE_PROBE_COUNT"
if [[ "$FAKE_SCENARIO" == negative-changed && "$count" -gt 1 ]]; then
  printf 'sha256:changed\n'
else
  printf 'sha256:baseline\n'
fi
"#;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "wamn-kubernetes-gate-runner-{}-{serial}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn executable(path: &Path, source: &str) {
    fs::write(path, source).expect("write fake executable");
    let mut permissions = fs::metadata(path).expect("fake metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fake executable");
}

fn positive_job(name: &str) -> String {
    json!({
        "name": name,
        "container": name,
        "expectation": "positive",
        "exit_code": 0,
        "image": EXPECTED_IMAGE,
        "log_contains": "overall PASS"
    })
    .to_string()
}

fn negative_job() -> String {
    json!({
        "name": "f2-testgate-refusal",
        "container": "f2-testgate-refusal",
        "expectation": "expected-negative",
        "exit_code": 1,
        "image": EXPECTED_IMAGE,
        "log_contains": "custom-node test gate (11.5): 1 case(s) FAILED"
    })
    .to_string()
}

struct RunResult {
    output: Output,
    receipt: Value,
    calls: String,
    probe_calls: String,
}

fn run(scenario: &str, jobs: &[String], negative: bool) -> RunResult {
    let directory = TestDirectory::new();
    let kubectl = directory.path("kubectl");
    let probe = directory.path("snapshot-probe");
    let manifest = directory.path("gate.yaml");
    let receipt = directory.path("receipt.json");
    let calls = directory.path("kubectl.calls");
    let probe_calls = directory.path("probe.calls");
    executable(&kubectl, FAKE_KUBECTL);
    executable(&probe, SNAPSHOT_PROBE);
    fs::write(&manifest, "apiVersion: batch/v1\nkind: Job\n").expect("write manifest");

    let mut command = Command::new("bash");
    command
        .arg(repository_root().join(RUNNER))
        .arg("--manifest")
        .arg(&manifest)
        .arg("--receipt")
        .arg(&receipt)
        .arg("--timeout-secs")
        .arg("3")
        .env("KUBECTL", &kubectl)
        .env("FAKE_SCENARIO", scenario)
        .env("FAKE_EXPECTED_IMAGE", EXPECTED_IMAGE)
        .env("FAKE_APPLIED", directory.path("applied"))
        .env("FAKE_CALLS", &calls)
        .env("FAKE_PROBE_CALLS", &probe_calls)
        .env("FAKE_PROBE_COUNT", directory.path("probe.count"));
    for job in jobs {
        command.arg("--job").arg(job);
    }
    if negative {
        command
            .arg("--snapshot-executable")
            .arg(&probe)
            .arg("--snapshot-arg")
            .arg("registry.example/v2/name:tag")
            .arg("--snapshot-arg")
            .arg("literal; never evaluated");
    }
    let output = command.output().expect("run verdict protocol");
    let receipt_bytes = fs::read(&receipt).unwrap_or_else(|error| {
        panic!(
            "receipt missing ({error}); stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    parse_receipt(&receipt_bytes).expect("receipt satisfies the typed SR26 consumer contract");
    RunResult {
        output,
        receipt: serde_json::from_slice(&receipt_bytes).expect("receipt JSON"),
        calls: fs::read_to_string(calls).unwrap_or_default(),
        probe_calls: fs::read_to_string(probe_calls).unwrap_or_default(),
    }
}

fn assert_red(scenario: &str, jobs: &[String], negative: bool, class: &str) {
    let result = run(scenario, jobs, negative);
    assert!(!result.output.status.success(), "{scenario} must be red");
    assert_eq!(result.receipt["verdict"], "fail");
    let classes = result.receipt["failure_classes"]
        .as_array()
        .expect("failure classes");
    assert!(
        classes
            .iter()
            .filter_map(Value::as_str)
            .any(|value| value.contains(class)),
        "{scenario} receipt lacks {class}: {}",
        result.receipt
    );
}

#[test]
fn fresh_positive_job_records_temporal_exit_and_image_evidence() {
    let result = run("pass", &[positive_job("gate")], false);
    assert!(
        result.output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert_eq!(
        result.receipt["protocol"],
        "wamn-kubernetes-gate-verdict/v1"
    );
    assert_eq!(result.receipt["verdict"], "pass");
    assert_eq!(
        result.receipt["jobs"][0]["observed"]["previous_uid"],
        "old-gate"
    );
    assert_eq!(result.receipt["jobs"][0]["observed"]["uid"], "new-gate");
    assert_eq!(
        result.receipt["jobs"][0]["observed"]["pods"][0]["container_exit_code"],
        0
    );
    assert_eq!(
        result.receipt["jobs"][0]["observed"]["pods"][0]["image_id"],
        "docker-pullable://wamn@sha256:abc"
    );
    let delete = result.calls.find("delete job gate").expect("fresh delete");
    let apply = result.calls.find("apply -f").expect("manifest apply");
    assert!(
        delete < apply,
        "Job deletion must precede apply: {}",
        result.calls
    );
}

#[test]
fn typed_receipt_distinguishes_positive_and_expected_negative_jobs() {
    let directory = TestDirectory::new();
    let receipt_path = directory.path("typed.json");
    let result = run("pass", &[positive_job("gate"), negative_job()], true);
    fs::write(&receipt_path, serde_json::to_vec(&result.receipt).unwrap()).unwrap();
    let receipt = parse_receipt(&fs::read(receipt_path).unwrap()).expect("typed receipt");
    assert_eq!(receipt.verdict, Verdict::Pass);
    assert_eq!(receipt.jobs[0].expectation, Expectation::Positive);
    assert_eq!(receipt.jobs[1].expectation, Expectation::ExpectedNegative);
}

#[test]
fn stale_job_mutant_is_red() {
    assert_red("stale", &[positive_job("gate")], false, "stale-job");
}

#[test]
fn never_started_pod_mutant_is_red() {
    assert_red(
        "never-start",
        &[positive_job("gate")],
        false,
        "pod-never-started",
    );
}

#[test]
fn init_container_failure_mutant_is_red() {
    assert_red(
        "init-failure",
        &[positive_job("gate")],
        false,
        "init-container-failed",
    );
}

#[test]
fn bounded_wait_timeout_mutant_is_red() {
    assert_red(
        "timeout",
        &[positive_job("gate")],
        false,
        "job-wait-timeout",
    );
}

#[test]
fn missing_logs_mutant_is_red() {
    assert_red(
        "missing-logs",
        &[positive_job("gate")],
        false,
        "logs-missing",
    );
}

#[test]
fn wrong_image_mutant_is_red() {
    assert_red("wrong-image", &[positive_job("gate")], false, "wrong-image");
}

#[test]
fn any_unready_job_makes_a_multi_job_aggregate_red() {
    assert_red(
        "multi-failure",
        &[positive_job("first"), positive_job("second")],
        false,
        "second:job-wait-timeout",
    );
}

#[test]
fn expected_negative_requires_typed_failure_and_an_unchanged_snapshot() {
    let result = run("pass", &[negative_job()], true);
    assert!(
        result.output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert_eq!(result.receipt["verdict"], "pass");
    assert_eq!(result.receipt["snapshot_probe"]["unchanged"], true);
    assert_eq!(
        result.receipt["snapshot_probe"]["before_stdout_sha256"],
        result.receipt["snapshot_probe"]["after_stdout_sha256"]
    );
    assert_eq!(
        result.receipt["snapshot_probe"]["argv"],
        json!(["registry.example/v2/name:tag", "literal; never evaluated"])
    );
    assert_eq!(
        result.probe_calls,
        "<registry.example/v2/name:tag>\n<literal; never evaluated>\n<registry.example/v2/name:tag>\n<literal; never evaluated>\n"
    );
}

#[test]
fn unexpected_negative_pass_mutant_is_red() {
    assert_red(
        "negative-pass",
        &[negative_job()],
        true,
        "condition-predates-run",
    );
}

#[test]
fn different_negative_failure_mutant_is_red() {
    assert_red(
        "negative-different",
        &[negative_job()],
        true,
        "container-exit-code-mismatch",
    );
}

#[test]
fn changed_negative_side_effect_mutant_is_red() {
    assert_red(
        "negative-changed",
        &[negative_job()],
        true,
        "negative-side-effect-changed",
    );
}

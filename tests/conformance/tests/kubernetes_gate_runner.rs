//! Machine-verdict protocol conformance over a deterministic fake `kubectl`.

use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use wamn_proof_conformance::kubernetes_gate_verdict::{Expectation, Verdict, parse_verdict_record};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

const RUNNER: &str = "tools/kubernetes-gate-run";
const EXPECTED_IMAGE: &str = "wamn-gates:dev";
const MAIN_IMAGE_ID: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SIDECAR_MANIFEST_ID: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SIDECAR_IMAGE: &str = "wamn-postgres:m1-pg18-720c455e";
const SIDECAR_RUNTIME_IMAGE_ID: &str = "docker.io/library/wamn-postgres@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SIDECAR_CONFIG_ID: &str =
    "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const SIDECAR_UPSTREAM_INDEX: &str =
    "sha256:ae6c78831cbc35fa3a4aaf4d763ddacf6183d6004774cc2dc28b3920410d1d1a";
const SIDECAR_UPSTREAM_CHILD: &str =
    "sha256:cd78ca58eb75f929698e117a589488ccb2bd45107247fe02400b50ff6c418324";

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
    if [[ "$kind" == nodes ]]; then
      if [[ "$FAKE_SCENARIO" == sidecar-node-set-mismatch ]]; then
        printf '{"items":[{"metadata":{"name":"wamn-control-plane"},"status":{"nodeInfo":{"architecture":"amd64"},"conditions":[{"type":"Ready","status":"True"}]}},{"metadata":{"name":"wamn-worker"},"status":{"nodeInfo":{"architecture":"amd64"},"conditions":[{"type":"Ready","status":"True"}]}}]}\n'
      else
        printf '{"items":[{"metadata":{"name":"wamn-control-plane"},"status":{"nodeInfo":{"architecture":"amd64"},"conditions":[{"type":"Ready","status":"True"}]}},{"metadata":{"name":"wamn-worker"},"status":{"nodeInfo":{"architecture":"amd64"},"conditions":[{"type":"Ready","status":"True"}]}},{"metadata":{"name":"wamn-worker2"},"status":{"nodeInfo":{"architecture":"amd64"},"conditions":[{"type":"Ready","status":"True"}]}}]}\n'
      fi
    elif [[ "$kind" == job ]]; then
      name=$1
      if [[ -f "$FAKE_DELETED" ]]; then
        if [[ "$FAKE_SCENARIO" == absence-probe-fail ]]; then exit 1; fi
        if [[ "$*" == *"--ignore-not-found=true"* ]]; then exit 0; fi
        exit 1
      fi
      if [[ ! -f "$FAKE_APPLIED" ]]; then
        printf '{"metadata":{"uid":"old-%s","creationTimestamp":"2000-01-01T00:00:00Z"}}\n' "$name"
        exit 0
      fi
      condition=Complete
      if [[ "$name" == *refusal* ]]; then condition=Failed; fi
      uid="new-$name"
      if [[ "$name" == m1-gate-generated ]]; then
        uid="12345678-1234-4abc-8def-1234567890ab"
      fi
      created=9998-01-01T00:00:00Z
      transition=9998-01-01T00:00:01Z
      if [[ "$FAKE_SCENARIO" == stale ]]; then
        uid="old-$name"
        created=2000-01-01T00:00:00Z
        transition=2000-01-01T00:00:01Z
      elif [[ "$FAKE_SCENARIO" == negative-pass && "$name" == *refusal* ]]; then
        condition=Complete
      fi
      conditions=$(printf '[{"type":"%s","status":"True","lastTransitionTime":"%s"}]' "$condition" "$transition")
      if [[ "$FAKE_SCENARIO" == timeout ]] || \
         [[ "$FAKE_SCENARIO" == multi-failure && "$name" == second ]]; then
        conditions='[]'
      fi
      printf '{"metadata":{"uid":"%s","creationTimestamp":"%s"},"status":{"conditions":%s}}\n' \
        "$uid" "$created" "$conditions"
    elif [[ "$kind" == pods ]]; then
      selector=$2
      if [[ "$selector" == batch.kubernetes.io/controller-uid=* ]]; then
        if [[ "$FAKE_SCENARIO" == pod-residue ]]; then
          printf '{"items":[{"metadata":{"name":"m1-residue"}}]}\n'
        else
          printf '{"items":[]}\n'
        fi
        exit 0
      fi
      name=${selector#job-name=}
      phase=Succeeded
      exit_code=0
      image="$FAKE_EXPECTED_IMAGE"
      image_id="docker-pullable://wamn@sha256:abc"
      if [[ "$FAKE_SCENARIO" == identity-match ]]; then
        image_id="docker.io/library/import@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
      fi
      started='"9998-01-01T00:00:00Z"'
      init='[{"name":"ready","state":{"terminated":{"exitCode":0,"finishedAt":"9998-01-01T00:00:00Z"}}}]'
      init_spec='[{"name":"ready"}]'
      if [[ "$name" == m1-gate-generated ]]; then
        sidecar_image="$FAKE_SIDECAR_IMAGE"
        sidecar_image_id='docker.io/library/wamn-postgres@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'
        sidecar_exit=0
        sidecar_finished='9998-01-01T00:00:03Z'
        if [[ "$FAKE_SCENARIO" == sidecar-wrong-image ]]; then sidecar_image='postgres:wrong'; fi
        if [[ "$FAKE_SCENARIO" == sidecar-image-id-missing ]]; then sidecar_image_id=''; fi
        if [[ "$FAKE_SCENARIO" == sidecar-image-id-mismatch ]]; then sidecar_image_id='docker.io/library/import@sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee'; fi
        if [[ "$FAKE_SCENARIO" == sidecar-failure ]]; then sidecar_exit=1; fi
        if [[ "$FAKE_SCENARIO" == sidecar-not-terminated ]]; then sidecar_finished=''; fi
        init_spec=$(printf '[{"name":"m1-postgres","image":"%s","restartPolicy":"Always"}]' "$sidecar_image")
        if [[ -n "$sidecar_finished" ]]; then
          init=$(printf '[{"name":"m1-postgres","imageID":"%s","state":{"terminated":{"exitCode":%s,"finishedAt":"%s"}}}]' "$sidecar_image_id" "$sidecar_exit" "$sidecar_finished")
        else
          init=$(printf '[{"name":"m1-postgres","imageID":"%s","state":{"running":{"startedAt":"9998-01-01T00:00:00Z"}}}]' "$sidecar_image_id")
        fi
      fi
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
      container=$name
      if [[ "$name" == m1-gate-generated ]]; then
        container=m1-gate
        image_id='docker.io/library/import@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
      fi
      printf '{"items":[{"metadata":{"name":"%s-pod","uid":"pod-%s","creationTimestamp":"9998-01-01T00:00:00Z"},"spec":{"nodeName":"wamn-worker","initContainers":%s,"containers":[{"name":"%s","image":"%s"}]},"status":{"startTime":%s,"phase":"%s","initContainerStatuses":%s,"containerStatuses":[{"name":"%s","imageID":"%s","state":{"terminated":{"exitCode":%s,"finishedAt":"9998-01-01T00:00:02Z"}}}]}}]}\n' \
        "$name" "$name" "$init_spec" "$container" "$image" "$started" "$phase" "$init" "$container" "$image_id" "$exit_code"
    fi
    ;;
  delete)
    if [[ "$FAKE_SCENARIO" == delete-fail && "$*" == *m1-gate-generated* ]]; then exit 1; fi
    if [[ "$FAKE_SCENARIO" != job-residue ]]; then : >"$FAKE_DELETED"; fi
    exit 0
    ;;
  apply) rm -f "$FAKE_DELETED"; : >"$FAKE_APPLIED" ;;
  create)
    if [[ "$*" == *"--dry-run=server"* ]]; then
      printf 'job.batch/m1-gate-dry-run\n'
    else
      rm -f "$FAKE_DELETED"
      : >"$FAKE_APPLIED"
      if [[ "$FAKE_SCENARIO" == generated-name-invalid ]]; then
        printf 'foreign-job'
      else
        printf 'm1-gate-generated'
      fi
    fi
    ;;
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
      printf 'expected negative gate refused the fixture\n'
    else
      printf 'overall PASS: true\n'
      if [[ "$name" == m1-gate-generated ]]; then
        record_uid='12345678-1234-4abc-8def-1234567890ab'
        record_suffix='1234567812344abc8def1234567890ab'
        if [[ "$FAKE_SCENARIO" == record-identity-mismatch ]]; then
          record_uid='22345678-1234-4abc-8def-1234567890ab'
          record_suffix='2234567812344abc8def1234567890ab'
        fi
        printf 'M1_RESOURCE_RECORD {"job_name":"m1-gate-generated","job_uid":"%s","suffix":"%s","project_database":"m1p","system_database":"m1y","cdc_role":"m1cdc","schema":"m1s","table":"receipts","publication":"m1pub","slot":"m1slot","stream":"M1","durable":"mat","report_dir":"/tmp/m1","org":"o","project":"p","environment":"e","tenant":"t","catalog":"c","flow":"f","registration":"r","entity":"x","root_run":"root","source_run":"source"}\n' "$record_uid" "$record_suffix"
        printf 'M1_MAIN_IMAGE_ID=sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n'
        entries=(
          'project-database=m1p' 'system-database=m1y'
          'cdc-role=m1cdc' 'cdc-role-login=m1cdc' 'cdc-role-sessions=m1cdc'
          'schema=m1s' 'table=receipts' 'publication=m1pub' 'slot=m1slot'
          'stream=M1' 'durable=mat' 'report-dir=/tmp/m1' 'org=o' 'project=p'
          'environment=e' 'tenant=t' 'catalog=c' 'flow=f' 'registration=r'
          'entity=x' 'root-run=root' 'source-run=source'
        )
        for phase in final external; do
          for entry in "${entries[@]}"; do
            kind=${entry%%=*}
            value=${entry#*=}
            if [[ "$FAKE_SCENARIO" == cleanup-missing && "$phase:$kind" == external:source-run ]]; then
              continue
            fi
            verdict=absent
            if [[ "$FAKE_SCENARIO" == cleanup-residue && "$phase:$kind" == external:cdc-role ]]; then
              verdict=leaked
            fi
            printf 'M1_CLEANUP phase=%s resource=%s name=%s verdict=%s\n' \
              "$phase" "$kind" "$value" "$verdict"
          done
          if [[ "$phase" == final ]]; then
            printf 'M1_CLEANUP phase=final resource=cdc-reader verdict=absent\n'
          fi
        done
      fi
      if [[ "$FAKE_SCENARIO" == identity-match ]]; then
        printf 'claimed-image-id=sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n'
      else
        printf 'claimed-image-id=docker-pullable://wamn@sha256:abc\n'
      fi
    fi
    ;;
  *) exit 64 ;;
esac
"#;

const FAKE_DOCKER: &str = r#"#!/usr/bin/env bash
set -uo pipefail
printf '%s\n' "$*" >>"$FAKE_DOCKER_CALLS"
[[ "$1" == exec ]] || exit 64
node=$2
shift 2
if [[ "$1 $2" == 'crictl inspecti' ]]; then
  if [[ "$FAKE_SCENARIO" == sidecar-inspect-failure && "$node" == wamn-worker ]]; then
    exit 1
  fi
  config='sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd'
  child='sha256:cd78ca58eb75f929698e117a589488ccb2bd45107247fe02400b50ff6c418324'
  if [[ "$FAKE_SCENARIO" == sidecar-label-disagreement && "$node" == wamn-worker ]]; then
    child='sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee'
  fi
  repo_tags='["docker.io/library/wamn-postgres:m1-pg18-720c455e"]'
  if [[ "$FAKE_SCENARIO" == sidecar-repotag-missing && "$node" == wamn-control-plane ]]; then
    repo_tags='[]'
  elif [[ "$FAKE_SCENARIO" == sidecar-repotag-wrong && "$node" == wamn-worker ]]; then
    repo_tags='["docker.io/library/foreign:m1-pg18"]'
  elif [[ "$FAKE_SCENARIO" == sidecar-repotag-duplicate && "$node" == wamn-worker2 ]]; then
    repo_tags='["docker.io/library/wamn-postgres:m1-pg18-720c455e","docker.io/library/wamn-postgres:m1-pg18-720c455e"]'
  fi
  printf '{"status":{"id":"%s","repoDigests":["docker.io/library/import-2026-08-15@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"],"repoTags":%s},"info":{"imageSpec":{"architecture":"amd64","config":{"Labels":{"wamn.dev/upstream-index":"%s","wamn.dev/upstream-child":"%s"}}}}}\n' \
    "$config" "$repo_tags" "$SIDECAR_UPSTREAM_INDEX" "$child"
elif [[ "$1 $2 $3 $4 $5" == 'ctr -n k8s.io images inspect' ]]; then
  if [[ "$FAKE_SCENARIO" == sidecar-ctr-inspect-failure && "$node" == wamn-worker ]]; then
    exit 1
  fi
  root=$6
  manifest='sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'
  if [[ "$FAKE_SCENARIO" == sidecar-ctr-root-mismatch && "$node" == wamn-control-plane ]]; then
    root='docker.io/library/foreign:m1-pg18'
  elif [[ "$FAKE_SCENARIO" == sidecar-ctr-manifest-wrong && "$node" == wamn-worker2 ]]; then
    manifest='sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee'
  fi
  printf '%s\n' "$root"
  printf '│    Created: 2026-08-15 20:59:42 +0000 UTC\n'
  printf '│    Updated: 2026-08-15 20:59:42 +0000 UTC\n'
  printf '│    Label "io.cri-containerd.image": "managed"\n'
  if [[ "$FAKE_SCENARIO" != sidecar-ctr-manifest-missing || "$node" != wamn-worker ]]; then
    printf '└── application/vnd.oci.image.manifest.v1+json @%s (2225 bytes)\n' "$manifest"
  fi
  if [[ "$FAKE_SCENARIO" == sidecar-ctr-manifest-ambiguous && "$node" == wamn-worker ]]; then
    printf '└── application/vnd.oci.image.manifest.v1+json @%s (2225 bytes)\n' "$manifest"
  fi
  config='sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd'
  if [[ "$FAKE_SCENARIO" == sidecar-ctr-config-mismatch && "$node" == wamn-worker2 ]]; then
    config='sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee'
  fi
  if [[ "$FAKE_SCENARIO" != sidecar-ctr-config-missing || "$node" != wamn-worker ]]; then
    printf '    ├── application/vnd.oci.image.config.v1+json @%s (10622 bytes)\n' "$config"
  fi
  if [[ "$FAKE_SCENARIO" == sidecar-ctr-config-ambiguous && "$node" == wamn-worker ]]; then
    printf '    ├── application/vnd.oci.image.config.v1+json @%s (10622 bytes)\n' "$config"
  fi
elif [[ "$1 $2 $3 $4 $5" == 'ctr -n k8s.io images check' ]]; then
  if [[ "$FAKE_SCENARIO" == sidecar-incomplete && "$node" == wamn-worker ]]; then
    exit 0
  fi
  printf '%s\n' "${7#name==}"
else
  exit 64
fi
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

const FAKE_M1_GATE: &str = r#"#!/usr/bin/env bash
set -uo pipefail
if [[ "$*" == *m1-cleanup* ]]; then
  printf 'cleanup\n' >>"$FAKE_EVENTS"
  exit 0
fi
trap 'printf "child-term\n" >>"$FAKE_EVENTS"; exit 143' HUP INT TERM
printf 'child-start\n' >>"$FAKE_EVENTS"
while :; do sleep 0.05; done
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

fn image_bound_job(name: &str, image_id: &str) -> String {
    json!({
        "name": name,
        "container": name,
        "expectation": "positive",
        "exit_code": 0,
        "image": EXPECTED_IMAGE,
        "log_contains": "overall PASS",
        "claimed_image_id": image_id,
        "claim_log_prefix": "claimed-image-id="
    })
    .to_string()
}

fn generated_job() -> String {
    json!({
        "name": "m1-gate-",
        "container": "m1-gate",
        "expectation": "positive",
        "exit_code": 0,
        "image": EXPECTED_IMAGE,
        "claimed_image_id": MAIN_IMAGE_ID,
        "claim_log_prefix": "M1_MAIN_IMAGE_ID=",
        "sidecar": "m1-postgres",
        "sidecar_image": SIDECAR_IMAGE,
        "sidecar_image_id": SIDECAR_MANIFEST_ID,
        "sidecar_config_id": SIDECAR_CONFIG_ID,
        "sidecar_upstream_index": SIDECAR_UPSTREAM_INDEX,
        "sidecar_upstream_child": SIDECAR_UPSTREAM_CHILD,
        "log_contains": "overall PASS"
    })
    .to_string()
}

fn negative_job() -> String {
    json!({
        "name": "expected-refusal",
        "container": "expected-refusal",
        "expectation": "expected-negative",
        "exit_code": 1,
        "image": EXPECTED_IMAGE,
        "log_contains": "expected negative gate refused the fixture"
    })
    .to_string()
}

struct RunResult {
    output: Output,
    record: Value,
    calls: String,
    probe_calls: String,
}

fn run(scenario: &str, jobs: &[String], negative: bool) -> RunResult {
    let directory = TestDirectory::new();
    let kubectl = directory.path("kubectl");
    let probe = directory.path("snapshot-probe");
    let manifest = directory.path("gate.yaml");
    let record = directory.path("record.json");
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
        .arg("--verdict-record")
        .arg(&record)
        .arg("--timeout-secs")
        .arg("3")
        .env("KUBECTL", &kubectl)
        .env("FAKE_SCENARIO", scenario)
        .env("FAKE_EXPECTED_IMAGE", EXPECTED_IMAGE)
        .env("FAKE_SIDECAR_IMAGE", SIDECAR_IMAGE)
        .env("FAKE_APPLIED", directory.path("applied"))
        .env("FAKE_DELETED", directory.path("deleted"))
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
    let record_bytes = fs::read(&record).unwrap_or_else(|error| {
        panic!(
            "record missing ({error}); stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    parse_verdict_record(&record_bytes).expect("record satisfies the typed SR26 consumer contract");
    RunResult {
        output,
        record: serde_json::from_slice(&record_bytes).expect("record JSON"),
        calls: fs::read_to_string(calls).unwrap_or_default(),
        probe_calls: fs::read_to_string(probe_calls).unwrap_or_default(),
    }
}

fn run_generated(scenario: &str) -> RunResult {
    let directory = TestDirectory::new();
    let kubectl = directory.path("kubectl");
    let docker = directory.path("docker");
    let manifest = directory.path("gate.yaml");
    let record = directory.path("record.json");
    let calls = directory.path("kubectl.calls");
    let docker_calls = directory.path("docker.calls");
    let sidecar_preflight_path = directory.path("sidecar-preflight.json");
    executable(&kubectl, FAKE_KUBECTL);
    executable(&docker, FAKE_DOCKER);
    fs::write(
        &manifest,
        "apiVersion: batch/v1\nkind: Job\nmetadata:\n  generateName: m1-gate-\n",
    )
    .expect("write manifest");
    let spec = generated_job();
    let output = Command::new("bash")
        .arg(repository_root().join(RUNNER))
        .arg("--manifest")
        .arg(&manifest)
        .arg("--verdict-record")
        .arg(&record)
        .arg("--timeout-secs")
        .arg("3")
        .arg("--generated-name-prefix")
        .arg("m1-gate-")
        .arg("--require-final-cleanup")
        .arg("--sidecar-preflight-record")
        .arg(&sidecar_preflight_path)
        .arg("--job")
        .arg(spec)
        .env("KUBECTL", &kubectl)
        .env("DOCKER", &docker)
        .env("FAKE_SCENARIO", scenario)
        .env("FAKE_EXPECTED_IMAGE", EXPECTED_IMAGE)
        .env("FAKE_SIDECAR_IMAGE", SIDECAR_IMAGE)
        .env("FAKE_APPLIED", directory.path("applied"))
        .env("FAKE_DELETED", directory.path("deleted"))
        .env("FAKE_CALLS", &calls)
        .env("FAKE_DOCKER_CALLS", &docker_calls)
        .env("SIDECAR_UPSTREAM_INDEX", SIDECAR_UPSTREAM_INDEX)
        .env("FAKE_PROBE_CALLS", directory.path("probe.calls"))
        .env("FAKE_PROBE_COUNT", directory.path("probe.count"))
        .output()
        .expect("run generated-name verdict protocol");
    let record_bytes = fs::read(&record).unwrap_or_else(|error| {
        panic!(
            "record missing ({error}); stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    parse_verdict_record(&record_bytes).expect("generated record satisfies typed contract");
    RunResult {
        output,
        record: serde_json::from_slice(&record_bytes).expect("record JSON"),
        calls: fs::read_to_string(calls).unwrap_or_default(),
        probe_calls: String::new(),
    }
}

fn run_rejected_sidecar_collection(scenario: &str) -> (Output, String, String, bool) {
    let directory = TestDirectory::new();
    let kubectl = directory.path("kubectl");
    let docker = directory.path("docker");
    let manifest = directory.path("gate.yaml");
    let record = directory.path("record.json");
    let calls = directory.path("kubectl.calls");
    let docker_calls = directory.path("docker.calls");
    let sidecar_preflight_path = directory.path("sidecar-preflight.json");
    executable(&kubectl, FAKE_KUBECTL);
    executable(&docker, FAKE_DOCKER);
    fs::write(
        &manifest,
        "apiVersion: batch/v1\nkind: Job\nmetadata:\n  generateName: m1-gate-\n",
    )
    .expect("write manifest");
    let output = Command::new("bash")
        .arg(repository_root().join(RUNNER))
        .arg("--manifest")
        .arg(&manifest)
        .arg("--verdict-record")
        .arg(&record)
        .arg("--timeout-secs")
        .arg("3")
        .arg("--generated-name-prefix")
        .arg("m1-gate-")
        .arg("--require-final-cleanup")
        .arg("--sidecar-preflight-record")
        .arg(&sidecar_preflight_path)
        .arg("--job")
        .arg(generated_job())
        .env("KUBECTL", &kubectl)
        .env("DOCKER", &docker)
        .env("FAKE_SCENARIO", scenario)
        .env("FAKE_EXPECTED_IMAGE", EXPECTED_IMAGE)
        .env("FAKE_SIDECAR_IMAGE", SIDECAR_IMAGE)
        .env("FAKE_APPLIED", directory.path("applied"))
        .env("FAKE_DELETED", directory.path("deleted"))
        .env("FAKE_CALLS", &calls)
        .env("FAKE_DOCKER_CALLS", &docker_calls)
        .env("SIDECAR_UPSTREAM_INDEX", SIDECAR_UPSTREAM_INDEX)
        .env("FAKE_PROBE_CALLS", directory.path("probe.calls"))
        .env("FAKE_PROBE_COUNT", directory.path("probe.count"))
        .output()
        .expect("run rejected sidecar collection");
    let preflight_exists = sidecar_preflight_path.exists();
    (
        output,
        fs::read_to_string(calls).unwrap_or_default(),
        fs::read_to_string(docker_calls).unwrap_or_default(),
        preflight_exists,
    )
}

fn assert_red(scenario: &str, jobs: &[String], negative: bool, class: &str) {
    let result = run(scenario, jobs, negative);
    assert!(!result.output.status.success(), "{scenario} must be red");
    assert_eq!(result.record["verdict"], "fail");
    let classes = result.record["failure_classes"]
        .as_array()
        .expect("failure classes");
    assert!(
        classes
            .iter()
            .filter_map(Value::as_str)
            .any(|value| value.contains(class)),
        "{scenario} record lacks {class}: {}",
        result.record
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
        result.record["protocol"],
        "wamn-kubernetes-gate-verdict/v0.1"
    );
    assert_eq!(result.record["verdict"], "pass");
    assert_eq!(
        result.record["jobs"][0]["observed"]["previous_uid"],
        "old-gate"
    );
    assert_eq!(result.record["jobs"][0]["observed"]["uid"], "new-gate");
    assert_eq!(
        result.record["jobs"][0]["observed"]["pods"][0]["container_exit_code"],
        0
    );
    assert_eq!(
        result.record["jobs"][0]["observed"]["pods"][0]["image_id"],
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
fn generated_name_job_is_created_captured_observed_and_deleted_exactly() {
    let result = run_generated("pass");
    assert!(
        result.output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert_eq!(result.record["verdict"], "pass");
    assert_eq!(result.record["jobs"][0]["name"], "m1-gate-generated");
    assert_eq!(
        result.record["jobs"][0]["observed"]["pods"][0]["node"],
        "wamn-worker"
    );
    assert_eq!(result.record["jobs"][0]["sidecar"], "m1-postgres");
    assert_eq!(result.record["jobs"][0]["claimed_image_id"], MAIN_IMAGE_ID);
    assert_eq!(
        result.record["jobs"][0]["observed"]["claimed_image_id"],
        MAIN_IMAGE_ID
    );
    assert_eq!(
        result.record["jobs"][0]["expected_sidecar_image"],
        SIDECAR_IMAGE
    );
    assert_eq!(
        result.record["jobs"][0]["expected_sidecar_image_id"],
        SIDECAR_MANIFEST_ID
    );
    assert_eq!(
        result.record["jobs"][0]["preflight_sidecar_config_id"],
        SIDECAR_CONFIG_ID
    );
    assert_eq!(
        result.record["jobs"][0]["sidecar_upstream_index"],
        SIDECAR_UPSTREAM_INDEX
    );
    assert_eq!(
        result.record["jobs"][0]["sidecar_upstream_child"],
        SIDECAR_UPSTREAM_CHILD
    );
    assert_eq!(
        result.record["jobs"][0]["sidecar_preflight_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        result.record["jobs"][0]["observed"]["pods"][0]["sidecar_exit_code"],
        0
    );
    assert_eq!(
        result.record["jobs"][0]["observed"]["pods"][0]["sidecar_image_id"],
        SIDECAR_RUNTIME_IMAGE_ID
    );
    assert!(!SIDECAR_IMAGE.contains('@'));
    assert_ne!(SIDECAR_CONFIG_ID, SIDECAR_MANIFEST_ID);
    let nodes = result
        .calls
        .find("get nodes -o json")
        .expect("runner-owned node enumeration");
    let dry_run = result
        .calls
        .find("create --dry-run=server -f")
        .expect("server-side dry-run");
    let create = result
        .calls
        .find("create -f")
        .expect("generated Job create");
    let delete = result
        .calls
        .rfind("delete job m1-gate-generated")
        .expect("exact generated Job delete");
    let absent = result
        .calls
        .rfind("get job m1-gate-generated -o name")
        .expect("exact generated Job absence probe");
    let pod_absent = result
        .calls
        .rfind("get pods -l batch.kubernetes.io/controller-uid=")
        .expect("exact generated Pod/sidecar absence probe");
    assert!(
        nodes < dry_run
            && dry_run < create
            && create < delete
            && delete < absent
            && absent < pod_absent
    );
    assert!(!result.calls.contains("apply -f"));
}

#[test]
fn generated_name_job_refuses_sidecar_collector_failures_before_create() {
    for scenario in [
        "sidecar-node-set-mismatch",
        "sidecar-ctr-inspect-failure",
        "sidecar-ctr-root-mismatch",
        "sidecar-ctr-manifest-missing",
        "sidecar-ctr-manifest-ambiguous",
        "sidecar-ctr-manifest-wrong",
        "sidecar-ctr-config-missing",
        "sidecar-ctr-config-ambiguous",
        "sidecar-ctr-config-mismatch",
        "sidecar-inspect-failure",
        "sidecar-incomplete",
        "sidecar-label-disagreement",
        "sidecar-repotag-missing",
        "sidecar-repotag-wrong",
        "sidecar-repotag-duplicate",
    ] {
        let (output, calls, docker_calls, preflight_exists) =
            run_rejected_sidecar_collection(scenario);
        assert!(!output.status.success(), "{scenario} must be red");
        assert!(
            !calls.contains("create --dry-run=server") && !calls.contains("create -f"),
            "{scenario} reached Job creation: {calls}"
        );
        assert!(
            !preflight_exists,
            "{scenario} left a partial preflight record"
        );
        if scenario == "sidecar-node-set-mismatch" {
            assert!(
                docker_calls.is_empty(),
                "invalid node set reached node inspection: {docker_calls}"
            );
        }
    }
}

#[test]
fn generated_name_job_requires_independent_cleanup_receipt() {
    for (scenario, class) in [
        ("cleanup-missing", "cleanup-external-receipt-set-mismatch"),
        ("cleanup-residue", "cleanup-external-receipt-set-mismatch"),
    ] {
        let result = run_generated(scenario);
        assert!(!result.output.status.success(), "{scenario} must be red");
        assert!(
            result.record["failure_classes"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .any(|failure| failure.contains(class)),
            "{}",
            result.record
        );
    }
}

#[test]
fn generated_name_job_refuses_invalid_identity_and_cleanup_residue() {
    for (scenario, class) in [
        ("generated-name-invalid", "generated-name-invalid"),
        (
            "record-identity-mismatch",
            "resource-record-identity-mismatch",
        ),
        ("delete-fail", "final-delete-failed"),
        ("absence-probe-fail", "final-absence-check-failed"),
        ("job-residue", "job-residue"),
        ("pod-residue", "pod-residue"),
        ("sidecar-wrong-image", "sidecar-wrong-image"),
        ("sidecar-image-id-missing", "sidecar-image-id-missing"),
        ("sidecar-image-id-mismatch", "sidecar-image-id-mismatch"),
        ("sidecar-failure", "sidecar-exit-code-mismatch"),
        ("sidecar-not-terminated", "sidecar-completion-invalid"),
    ] {
        let result = run_generated(scenario);
        assert!(!result.output.status.success(), "{scenario} must be red");
        assert!(
            result.record["failure_classes"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .any(|failure| failure.contains(class)),
            "{}",
            result.record
        );
        if scenario == "generated-name-invalid" {
            assert!(
                result.calls.contains("delete job foreign-job --wait=true"),
                "created wrong-prefix Job must be deleted by its exact returned name: {}",
                result.calls
            );
            assert!(
                result.calls.contains(
                    "get pods -l batch.kubernetes.io/controller-uid=new-foreign-job -o json"
                ),
                "created wrong-prefix Job must receive an exact UID Pod absence probe: {}",
                result.calls
            );
        }
    }
}

#[test]
fn manifest_pid_one_forwards_term_then_runs_cleanup_once() {
    let directory = TestDirectory::new();
    let fake_gate = directory.path("wamn-gates");
    let events = directory.path("events");
    executable(&fake_gate, FAKE_M1_GATE);
    let manifest = fs::read_to_string(repository_root().join("deploy/gates/m1-gate-job.yaml"))
        .expect("read M1 Job manifest");
    let gate_container = manifest
        .split_once("        - name: m1-gate\n")
        .map(|(_, tail)| tail)
        .expect("extract M1 gate container");
    let block = gate_container
        .split_once("            - |\n")
        .and_then(|(_, tail)| tail.split_once("          env:\n"))
        .map(|(block, _)| block)
        .expect("extract PID1 shell block");
    let script = block
        .lines()
        .map(|line| line.strip_prefix("              ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
        .replace("/usr/local/bin/wamn-gates", fake_gate.to_str().unwrap());
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .env("FAKE_EVENTS", &events)
        .spawn()
        .expect("spawn manifest PID1 shell");
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if fs::read_to_string(&events)
            .unwrap_or_default()
            .contains("child-start")
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        fs::read_to_string(&events)
            .unwrap_or_default()
            .contains("child-start"),
        "gate child did not start"
    );
    let signal = Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()
        .expect("signal manifest shell");
    assert!(signal.success());
    let status = child.wait().expect("wait for manifest shell");
    assert_eq!(status.code(), Some(143));
    let observed = fs::read_to_string(events).expect("signal receipt");
    assert!(observed.contains("child-term"), "{observed}");
    assert_eq!(observed.matches("cleanup\n").count(), 1, "{observed}");
}

#[test]
fn typed_record_distinguishes_positive_and_expected_negative_jobs() {
    let directory = TestDirectory::new();
    let record_path = directory.path("typed.json");
    let result = run("pass", &[positive_job("gate"), negative_job()], true);
    fs::write(&record_path, serde_json::to_vec(&result.record).unwrap()).unwrap();
    let record = parse_verdict_record(&fs::read(record_path).unwrap()).expect("typed record");
    assert_eq!(record.verdict, Verdict::Pass);
    assert_eq!(record.jobs[0].expectation, Expectation::Positive);
    assert_eq!(record.jobs[1].expectation, Expectation::ExpectedNegative);
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
fn claimed_image_id_must_equal_the_log_claim_and_observed_runtime_id() {
    let observed = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    assert_red(
        "pass",
        &[image_bound_job("gate", observed)],
        false,
        "claimed-image-id-mismatch",
    );
}

#[test]
fn exact_claimed_and_observed_image_id_match_is_green() {
    let image_id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let result = run(
        "identity-match",
        &[image_bound_job("gate", image_id)],
        false,
    );
    assert!(
        result.output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert_eq!(result.record["jobs"][0]["claimed_image_id"], image_id);
    assert_eq!(
        result.record["jobs"][0]["observed"]["claimed_image_id"],
        image_id
    );
    assert_eq!(
        result.record["jobs"][0]["observed"]["pods"][0]["image_id"],
        format!("docker.io/library/import@{image_id}")
    );
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
fn opposite_terminal_condition_is_reported_without_waiting_for_timeout() {
    assert_red(
        "negative-pass",
        &[negative_job()],
        true,
        "unexpected-terminal-condition",
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
    assert_eq!(result.record["verdict"], "pass");
    assert_eq!(result.record["snapshot_probe"]["unchanged"], true);
    assert_eq!(
        result.record["snapshot_probe"]["before_stdout_sha256"],
        result.record["snapshot_probe"]["after_stdout_sha256"]
    );
    assert_eq!(
        result.record["snapshot_probe"]["argv"],
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

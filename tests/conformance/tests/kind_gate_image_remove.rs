//! Exact kind gate-image cleanup over deterministic fake CLIs.

use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

const TOOL: &str = "tools/kind-gate-image-remove";
const SELECTED: &str = "wamn-gates:cf-wave1-0123456789abcdef";
const CANONICAL: &str = "docker.io/library/wamn-gates:cf-wave1-0123456789abcdef";
const SECOND_SELECTED: &str = "wamn-gates:cf-wave2-fedcba9876543210";
const SECOND_CANONICAL: &str = "docker.io/library/wamn-gates:cf-wave2-fedcba9876543210";
const RETAINED: &str = "docker.io/library/wamn-gates:cf-wave2-retained";
const CONFIG_ID: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TARGET_DIGEST: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const IMPORT_ALIAS: &str =
    "import-2026-08-05@sha256:1111111111111111111111111111111111111111111111111111111111111111";

const FAKE_KIND: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf 'kind' >>"$FAKE_CALLS"
printf '\t%q' "$@" >>"$FAKE_CALLS"
printf '\n' >>"$FAKE_CALLS"
[[ "$*" == "get nodes --name wamn" ]] || exit 64
if [[ "${FAKE_SCENARIO:-}" != no-nodes ]]; then
  printf 'node-a\nnode-b\n'
fi
"#;

const FAKE_KUBECTL: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf 'kubectl' >>"$FAKE_CALLS"
printf '\t%q' "$@" >>"$FAKE_CALLS"
printf '\n' >>"$FAKE_CALLS"
case " $* " in
  *" get pods "*) cat "$FAKE_STATE/pods.json" ;;
  *" get deployments,statefulsets,daemonsets,replicasets,jobs,cronjobs "*)
    cat "$FAKE_STATE/workloads.json"
    ;;
  *) exit 64 ;;
esac
"#;

const FAKE_DOCKER: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf 'docker' >>"$FAKE_CALLS"
printf '\t%q' "$@" >>"$FAKE_CALLS"
printf '\n' >>"$FAKE_CALLS"
[[ ${1-} == exec && -n ${2-} ]] || exit 64
node=$2
shift 2
images="$FAKE_STATE/$node.images"
cri_images="$FAKE_STATE/$node.cri-images.json"
containers="$FAKE_STATE/$node.containers.json"

if [[ "$*" == "crictl images -o json" ]]; then
  cat "$cri_images"
elif [[ "$*" == "crictl ps -a -o json" ]]; then
  cat "$containers"
elif [[ "$*" == "ctr -n k8s.io images ls" ]]; then
  printf 'REF TYPE DIGEST SIZE PLATFORMS LABELS\n'
  while IFS=$'\t' read -r reference digest; do
    [[ -n "$reference" ]] || continue
    printf '%s application/vnd.oci.image.manifest.v1+json %s 1.0MiB linux/amd64 -\n' \
      "$reference" "$digest"
  done <"$images"
elif [[ "$*" == "ctr -n k8s.io images ls -q" ]]; then
  cut -f1 "$images"
elif [[ ${1-} == crictl && ${2-} == rm && -n ${3-} ]]; then
  container_id=$3
  if [[ "${FAKE_SCENARIO:-}" == remove-failure && "$node" == node-b ]]; then
    exit 1
  fi
  temporary="$containers.tmp"
  jq --arg id "$container_id" '.containers |= map(select(.id != $id))' \
    "$containers" >"$temporary"
  mv "$temporary" "$containers"
elif [[ ${1-} == ctr && ${2-} == -n && ${3-} == k8s.io &&
        ${4-} == images && ${5-} == rm && ${6-} == --sync ]]; then
  shift 6
  if [[ "${FAKE_SCENARIO:-}" == remove-failure && "$node" == node-b ]]; then
    exit 1
  fi
  if [[ "${FAKE_SCENARIO:-}" != verification-failure ]]; then
    for reference in "$@"; do
      temporary="$images.tmp"
      awk -F '\t' -v reference="$reference" '$1 != reference' \
        "$images" >"$temporary"
      mv "$temporary" "$images"
      if [[ "$reference" == docker.io/library/wamn-gates:* ]]; then
        temporary="$cri_images.tmp"
        jq --arg reference "$reference" '
          .images |= map(.repoTags |= map(select(. != $reference)))
          | .images |= map(select(.repoTags | length > 0))
        ' "$cri_images" >"$temporary"
        mv "$temporary" "$cri_images"
      fi
    done
  fi
else
  exit 64
fi
"#;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "wamn-kind-gate-image-remove-{}-{serial}",
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

fn setup(retained: bool, containers: &[Value]) -> TestDirectory {
    let directory = TestDirectory::new();
    executable(&directory.path("kind"), FAKE_KIND);
    executable(&directory.path("kubectl"), FAKE_KUBECTL);
    executable(&directory.path("docker"), FAKE_DOCKER);
    fs::write(directory.path("calls"), "").expect("create call log");
    fs::write(directory.path("pods.json"), r#"{"items":[]}"#).expect("write Pods");
    fs::write(directory.path("workloads.json"), r#"{"items":[]}"#).expect("write workloads");

    let mut tags = vec![CANONICAL];
    if retained {
        tags.push(RETAINED);
    }
    let cri_images = json!({
        "images": [{
            "id": CONFIG_ID,
            "repoTags": tags,
            "repoDigests": [],
            "size": "1048576"
        }]
    });
    for node in ["node-a", "node-b"] {
        let mut image_lines = format!(
            "{CANONICAL}\t{TARGET_DIGEST}\n{CONFIG_ID}\t{TARGET_DIGEST}\n{IMPORT_ALIAS}\t{TARGET_DIGEST}\n"
        );
        if retained {
            image_lines.push_str(&format!("{RETAINED}\t{TARGET_DIGEST}\n"));
        }
        image_lines.push_str(
            "docker.io/library/unrelated:keep\tsha256:2222222222222222222222222222222222222222222222222222222222222222\n",
        );
        fs::write(directory.path(&format!("{node}.images")), image_lines)
            .expect("write image inventory");
        fs::write(
            directory.path(&format!("{node}.cri-images.json")),
            serde_json::to_vec(&cri_images).expect("serialize CRI images"),
        )
        .expect("write CRI images");
        let node_containers = if node == "node-a" { containers } else { &[] };
        fs::write(
            directory.path(&format!("{node}.containers.json")),
            serde_json::to_vec(&json!({ "containers": node_containers }))
                .expect("serialize containers"),
        )
        .expect("write containers");
    }
    directory
}

fn add_second_selected_tag(directory: &TestDirectory) {
    for node in ["node-a", "node-b"] {
        let image_path = directory.path(&format!("{node}.images"));
        let mut images = fs::read_to_string(&image_path).expect("read image inventory");
        images.push_str(&format!("{SECOND_CANONICAL}\t{TARGET_DIGEST}\n"));
        fs::write(image_path, images).expect("extend image inventory");

        let cri_path = directory.path(&format!("{node}.cri-images.json"));
        let mut cri: Value =
            serde_json::from_slice(&fs::read(&cri_path).expect("read CRI images for extension"))
                .expect("parse CRI images for extension");
        cri["images"][0]["repoTags"]
            .as_array_mut()
            .expect("repoTags array")
            .push(json!(SECOND_CANONICAL));
        fs::write(
            cri_path,
            serde_json::to_vec(&cri).expect("serialize extended CRI images"),
        )
        .expect("write extended CRI images");
    }
}

fn run(directory: &TestDirectory, scenario: &str, arguments: &[&str]) -> Output {
    Command::new("bash")
        .arg(repository_root().join(TOOL))
        .args(arguments)
        .env("KIND", directory.path("kind"))
        .env("KUBECTL", directory.path("kubectl"))
        .env("DOCKER", directory.path("docker"))
        .env("FAKE_CALLS", directory.path("calls"))
        .env("FAKE_STATE", &directory.0)
        .env("FAKE_SCENARIO", scenario)
        .output()
        .expect("run kind gate-image remover")
}

fn calls(directory: &TestDirectory) -> String {
    fs::read_to_string(directory.path("calls")).expect("read call log")
}

fn image_state(directory: &TestDirectory, node: &str) -> String {
    fs::read_to_string(directory.path(&format!("{node}.images"))).expect("read image state")
}

#[test]
fn dry_run_plans_every_node_without_mutation() {
    let directory = setup(false, &[]);
    let output = run(&directory, "", &["--image", SELECTED]);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("SUMMARY mode=dry-run cluster=wamn nodes=2 selected=1"));
    assert_eq!(stdout.matches("remove-image=").count(), 6);
    assert!(image_state(&directory, "node-a").contains(CANONICAL));
    assert!(!calls(&directory).contains("images\trm"));
}

#[test]
fn apply_removes_selected_tags_and_internal_aliases_only() {
    let directory = setup(false, &[]);
    let output = run(&directory, "", &["--image", SELECTED, "--apply"]);
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    for node in ["node-a", "node-b"] {
        let state = image_state(&directory, node);
        assert!(!state.contains(CANONICAL));
        assert!(!state.contains(CONFIG_ID));
        assert!(!state.contains(IMPORT_ALIAS));
        assert!(state.contains("docker.io/library/unrelated:keep"));
    }
    let calls = calls(&directory);
    assert_eq!(
        calls
            .matches(&format!("images\trm\t--sync\t{CANONICAL}"))
            .count(),
        2
    );
    assert!(!calls.contains("unrelated:keep"));
}

#[test]
fn apply_removes_an_entire_explicitly_selected_multi_tag_group() {
    let directory = setup(false, &[]);
    add_second_selected_tag(&directory);
    let output = run(
        &directory,
        "",
        &["--image", SELECTED, "--image", SECOND_SELECTED, "--apply"],
    );
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    for node in ["node-a", "node-b"] {
        let state = image_state(&directory, node);
        assert!(!state.contains(CANONICAL));
        assert!(!state.contains(SECOND_CANONICAL));
        assert!(state.contains("docker.io/library/unrelated:keep"));
    }
}

#[test]
fn protected_and_shared_tags_are_refused_before_mutation() {
    let protected = setup(false, &[]);
    let protected_output = run(&protected, "", &["--image", "wamn-gates:dev", "--apply"]);
    assert_eq!(protected_output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&protected_output.stderr).contains("protected gate image"));
    assert!(!calls(&protected).contains("docker\texec"));

    let shared = setup(true, &[]);
    let shared_output = run(&shared, "", &["--image", SELECTED, "--apply"]);
    assert!(!shared_output.status.success());
    assert!(String::from_utf8_lossy(&shared_output.stderr).contains("reason=shared-content"));
    assert!(!calls(&shared).contains("images\trm"));
    assert!(image_state(&shared, "node-a").contains(RETAINED));
}

#[test]
fn kubernetes_workload_reference_is_a_hard_blocker() {
    let directory = setup(false, &[]);
    let workloads = json!({
        "items": [{
            "kind": "Job",
            "metadata": { "namespace": "wamn-system", "name": "retained-proof" },
            "spec": {
                "template": {
                    "spec": { "containers": [{ "image": SELECTED }] }
                }
            }
        }]
    });
    fs::write(
        directory.path("workloads.json"),
        serde_json::to_vec(&workloads).expect("serialize workload"),
    )
    .expect("write workload");
    let output = run(&directory, "", &["--image", SELECTED, "--apply"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("kubernetes-reference"));
    assert!(!calls(&directory).contains("images\trm"));
}

#[test]
fn malformed_workload_inventory_fails_closed() {
    let directory = setup(false, &[]);
    let workloads = json!({
        "items": [{
            "kind": "Job",
            "metadata": { "namespace": "wamn-system", "name": "malformed" },
            "spec": {
                "template": {
                    "spec": { "containers": [{ "image": { "unexpected": "object" } }] }
                }
            }
        }]
    });
    fs::write(
        directory.path("workloads.json"),
        serde_json::to_vec(&workloads).expect("serialize malformed workload"),
    )
    .expect("write malformed workload");
    let output = run(&directory, "", &["--image", SELECTED, "--apply"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("failed to parse Kubernetes workload image references")
    );
    assert!(!calls(&directory).contains("docker\texec"));
}

#[test]
fn empty_cluster_inventory_is_refused_before_mutation() {
    let directory = setup(false, &[]);
    let output = run(&directory, "no-nodes", &["--image", SELECTED, "--apply"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("has no nodes"));
    assert!(!calls(&directory).contains("docker\texec"));
}

#[test]
fn live_running_and_unattributed_containers_are_hard_blockers() {
    let live_container = json!({
        "id": "live-container",
        "image": { "image": CONFIG_ID, "userSpecifiedImage": SELECTED },
        "imageId": CONFIG_ID,
        "imageRef": CONFIG_ID,
        "state": "CONTAINER_EXITED",
        "labels": { "io.kubernetes.pod.uid": "live-pod" }
    });
    let live = setup(false, &[live_container]);
    let pods = json!({
        "items": [{
            "metadata": {
                "uid": "live-pod",
                "namespace": "wamn-system",
                "name": "still-visible"
            },
            "spec": { "containers": [{ "image": "wamn-gates:dev" }] }
        }]
    });
    fs::write(
        live.path("pods.json"),
        serde_json::to_vec(&pods).expect("serialize live Pod"),
    )
    .expect("write live Pod");
    let live_output = run(
        &live,
        "",
        &[
            "--image",
            SELECTED,
            "--apply",
            "--remove-orphaned-containers",
        ],
    );
    assert!(!live_output.status.success());
    assert!(String::from_utf8_lossy(&live_output.stderr).contains("pod-still-exists"));
    assert!(!calls(&live).contains("crictl\trm"));

    let running_container = json!({
        "id": "running-container",
        "image": { "image": CONFIG_ID, "userSpecifiedImage": SELECTED },
        "imageId": CONFIG_ID,
        "imageRef": CONFIG_ID,
        "state": "CONTAINER_RUNNING",
        "labels": { "io.kubernetes.pod.uid": "deleted-pod" }
    });
    let missing_uid_container = json!({
        "id": "missing-uid-container",
        "image": { "image": CONFIG_ID, "userSpecifiedImage": SELECTED },
        "imageId": CONFIG_ID,
        "imageRef": CONFIG_ID,
        "state": "CONTAINER_EXITED",
        "labels": {}
    });
    let unsafe_containers = setup(false, &[running_container, missing_uid_container]);
    let unsafe_output = run(
        &unsafe_containers,
        "",
        &[
            "--image",
            SELECTED,
            "--apply",
            "--remove-orphaned-containers",
        ],
    );
    assert!(!unsafe_output.status.success());
    let stderr = String::from_utf8_lossy(&unsafe_output.stderr);
    assert!(stderr.contains("container-state-CONTAINER_RUNNING"));
    assert!(stderr.contains("missing-pod-uid"));
    assert!(!calls(&unsafe_containers).contains("crictl\trm"));
}

#[test]
fn same_config_container_from_unselected_tag_is_not_removed() {
    let container = json!({
        "id": "retained-container",
        "image": { "image": CONFIG_ID, "userSpecifiedImage": "wamn-gates:dev" },
        "imageId": CONFIG_ID,
        "imageRef": CONFIG_ID,
        "state": "CONTAINER_EXITED",
        "labels": { "io.kubernetes.pod.uid": "deleted-pod" }
    });
    let directory = setup(false, &[container]);
    let output = run(
        &directory,
        "",
        &[
            "--image",
            SELECTED,
            "--apply",
            "--remove-orphaned-containers",
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("container-image-not-selected"));
    assert!(!calls(&directory).contains("crictl\trm"));
    assert!(image_state(&directory, "node-a").contains(CANONICAL));
}

#[test]
fn malformed_cri_container_inventory_fails_closed() {
    let container = json!({
        "id": "malformed-container",
        "image": "not-an-image-object",
        "imageId": CONFIG_ID,
        "imageRef": CONFIG_ID,
        "state": "CONTAINER_EXITED",
        "labels": { "io.kubernetes.pod.uid": "deleted-pod" }
    });
    let directory = setup(false, &[container]);
    let output = run(
        &directory,
        "",
        &[
            "--image",
            SELECTED,
            "--apply",
            "--remove-orphaned-containers",
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed to parse CRI containers"));
    assert!(!calls(&directory).contains("crictl\trm"));
    assert!(!calls(&directory).contains("images\trm"));
}

#[test]
fn orphaned_container_removal_requires_separate_authorization() {
    let container = json!({
        "id": "container-a",
        "image": { "image": CONFIG_ID, "userSpecifiedImage": SELECTED },
        "imageId": CONFIG_ID,
        "imageRef": CONFIG_ID,
        "state": "CONTAINER_EXITED",
        "labels": { "io.kubernetes.pod.uid": "deleted-pod" }
    });
    let directory = setup(false, &[container]);
    let refused = run(&directory, "", &["--image", SELECTED, "--apply"]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("reason=orphan-removal-not-authorized")
    );
    assert!(!calls(&directory).contains("crictl\trm"));

    let applied = run(
        &directory,
        "",
        &[
            "--image",
            SELECTED,
            "--apply",
            "--remove-orphaned-containers",
        ],
    );
    assert!(
        applied.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert!(calls(&directory).contains("crictl\trm\tcontainer-a"));
}

#[test]
fn verification_failure_is_reported_after_all_nodes_are_attempted() {
    let directory = setup(false, &[]);
    let output = run(
        &directory,
        "verification-failure",
        &["--image", SELECTED, "--apply"],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("verification failed: image reference remains on node-a"));
    assert!(stderr.contains("verification failed: image reference remains on node-b"));
    assert!(stderr.contains("kind gate-image cleanup failed verification"));
}

#[test]
fn alias_removal_failure_preserves_selected_tag_for_retry() {
    let directory = setup(false, &[]);
    let output = run(
        &directory,
        "remove-failure",
        &["--image", SELECTED, "--apply"],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("preserving selected image tags on node-b"));
    assert!(!image_state(&directory, "node-a").contains(CANONICAL));
    assert!(image_state(&directory, "node-b").contains(CANONICAL));
    assert!(
        !calls(&directory)
            .lines()
            .any(|line| line.contains("node-b") && line.ends_with(CANONICAL))
    );

    let retry = run(&directory, "", &["--image", SELECTED, "--apply"]);
    assert!(
        retry.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert!(!image_state(&directory, "node-b").contains(CANONICAL));
}

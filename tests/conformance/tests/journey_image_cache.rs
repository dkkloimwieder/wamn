//! Exact source identity, reuse, leases and retention over a fake Docker.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

const TOOL: &str = "tools/journey-image-cache";
const HEAD_LABEL: &str = "0123456789abcdef0123456789abcdef01234567";
const CLUSTER: &str = "wamn-receiving-00000000000000000000000000000001";

/// Reports every image as absent, so `ensure` always reaches the build.
const FAKE_DOCKER_ABSENT: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf 'docker' >>"$FAKE_CALLS"
printf '\t%q' "$@" >>"$FAKE_CALLS"
printf '\n' >>"$FAKE_CALLS"
if [[ $1 == build ]]; then
  context=${!#}
  [[ -f $context/Dockerfile ]] && cat "$context/Dockerfile" >>"$FAKE_CALLS"
fi
case "$1 ${2-}" in
  "image inspect") exit 1 ;;
esac
exit 0
"#;

/// Reports every image as present, so `ensure` must not build one.
const FAKE_DOCKER_PRESENT: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf 'docker' >>"$FAKE_CALLS"
printf '\t%q' "$@" >>"$FAKE_CALLS"
printf '\n' >>"$FAKE_CALLS"
if [[ $1 == build ]]; then
  context=${!#}
  [[ -f $context/Dockerfile ]] && cat "$context/Dockerfile" >>"$FAKE_CALLS"
fi
exit 0
"#;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "wamn-journey-image-cache-{}-{serial}",
            std::process::id()
        ));
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
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_path_buf()
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write fake executable");
    let mut permissions = fs::metadata(path)
        .expect("read fake permissions")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set fake permissions");
}

fn git(repository: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(repository)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A repository carrying every path the tool hashes, and nothing else.
fn source_repository(directory: &TestDirectory) -> PathBuf {
    let repository = directory.path("source");
    fs::create_dir_all(repository.join(".cargo")).expect("create .cargo");
    for name in [
        "crates",
        "apps",
        "services",
        "test-support",
        "tests",
        "deploy",
    ] {
        let path = repository.join(name);
        fs::create_dir_all(&path).expect("create copied directory");
        fs::write(path.join("kept.txt"), "one\n").expect("write copied file");
    }
    for (name, body) in [
        ("Dockerfile", "FROM scratch\n"),
        (".dockerignore", "/target\n"),
        ("Cargo.toml", "[workspace]\n"),
        ("Cargo.lock", "version = 4\n"),
        (".cargo/config.toml", "[build]\n"),
    ] {
        fs::write(repository.join(name), body).expect("write copied file");
    }
    fs::create_dir_all(repository.join("docs")).expect("create uncopied directory");
    fs::write(repository.join("docs/note.md"), "one\n").expect("write uncopied file");

    git(&repository, &["init", "--quiet"]);
    git(
        &repository,
        &["config", "user.email", "test@example.invalid"],
    );
    git(&repository, &["config", "user.name", "test"]);
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "--quiet", "-m", "one"]);
    repository
}

fn run(directory: &TestDirectory, arguments: &[&str]) -> Output {
    Command::new("bash")
        .arg(repository_root().join(TOOL))
        .args(arguments)
        .env("DOCKER", directory.path("docker"))
        .env("FAKE_CALLS", directory.path("calls"))
        .env("XDG_CACHE_HOME", directory.path("cache"))
        .output()
        .expect("run the journey image cache")
}

fn identity(directory: &TestDirectory, repository: &Path) -> String {
    stage_identity(directory, repository, "")
}

fn stage_identity(directory: &TestDirectory, repository: &Path, target: &str) -> String {
    let output = run(
        directory,
        &["identity", repository.to_str().expect("path"), target],
    );
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn calls(directory: &TestDirectory) -> String {
    fs::read_to_string(directory.path("calls")).unwrap_or_default()
}

fn state(directory: &TestDirectory, kind: &str, name: &str) -> PathBuf {
    directory.path(&format!("cache/wamn-journey-images/{kind}/{name}"))
}

/// The identity answers one question: does this source produce the same image?
///
/// A commit that edits only an uncopied path produces the same image, and two
/// runs of it must find each other's work. A commit that edits a copied path
/// produces a different image and must not (wamn-szr0).
#[test]
fn the_identity_follows_the_copied_paths_and_ignores_the_rest() {
    let directory = TestDirectory::new();
    executable(&directory.path("docker"), FAKE_DOCKER_ABSENT);
    let repository = source_repository(&directory);

    let first = identity(&directory, &repository);
    assert_eq!(first.len(), 16, "the identity is a short stable hash");
    assert_eq!(first, identity(&directory, &repository), "identity drifted");

    fs::write(repository.join("docs/note.md"), "two\n").expect("edit the uncopied file");
    git(&repository, &["commit", "--quiet", "-am", "uncopied"]);
    assert_eq!(
        first,
        identity(&directory, &repository),
        "an uncopied path changed the identity"
    );

    fs::write(repository.join("crates/kept.txt"), "two\n").expect("edit the copied file");
    git(&repository, &["commit", "--quiet", "-am", "copied"]);
    assert_ne!(
        first,
        identity(&directory, &repository),
        "a copied path left the identity unchanged"
    );
}

/// The copied directories that hold only test crates and test data. The tool's
/// TEST_ONLY list names the same paths.
const TEST_ONLY: [&str; 7] = [
    "tests/",
    "test-support/fixture-package/",
    "test-support/fixtures/",
    "test-support/harness/",
    "test-support/infrastructure/",
    "test-support/simulator/",
    "apps/receiving/tests/",
];

/// A commit that changes only test files keeps the host and identity images,
/// so a rerun after a test fix rebuilds nothing. A test manifest still counts,
/// and the gates image, which carries the test binaries, still rebuilds
/// (wamn-as5u).
#[test]
fn the_host_and_identity_stages_ignore_test_only_files() {
    let directory = TestDirectory::new();
    executable(&directory.path("docker"), FAKE_DOCKER_ABSENT);
    let repository = source_repository(&directory);
    for path in TEST_ONLY {
        fs::create_dir_all(repository.join(path)).expect("create the test directory");
        fs::write(repository.join(path).join("kept.rs"), "one\n").expect("write test file");
        fs::write(repository.join(path).join("Cargo.toml"), "[package]\n")
            .expect("write test manifest");
    }
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "--quiet", "-m", "tests"]);
    let host = stage_identity(&directory, &repository, "host");
    let gates = stage_identity(&directory, &repository, "gates");
    assert_eq!(host, stage_identity(&directory, &repository, "identity"));

    for path in TEST_ONLY {
        fs::write(repository.join(path).join("kept.rs"), "two\n").expect("edit test file");
    }
    git(&repository, &["commit", "--quiet", "-am", "test only"]);
    assert_eq!(
        host,
        stage_identity(&directory, &repository, "host"),
        "a test-only change rebuilt the host image"
    );
    assert_ne!(
        gates,
        stage_identity(&directory, &repository, "gates"),
        "a test-only change kept the gates image"
    );

    fs::write(repository.join("tests/Cargo.toml"), "[package]\n# two\n")
        .expect("edit test manifest");
    git(&repository, &["commit", "--quiet", "-am", "manifest"]);
    assert_ne!(
        host,
        stage_identity(&directory, &repository, "host"),
        "a test manifest change kept the host image"
    );
}

/// The host and identity binaries build from none of the test-only paths, so
/// leaving those paths out of their identity cannot hide a change to them.
#[test]
fn the_host_and_identity_binaries_reach_no_test_only_path() {
    let root = repository_root();
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(&root)
        .args(["metadata", "--locked", "--offline", "--format-version", "1"])
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("decode cargo metadata");
    let packages = metadata["packages"].as_array().expect("packages");
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolve nodes");
    for binary in ["wamn-host", "wamn-identity"] {
        let mut stack = packages
            .iter()
            .filter(|package| package["name"] == binary && package["source"].is_null())
            .map(|package| package["id"].clone())
            .collect::<Vec<_>>();
        assert_eq!(stack.len(), 1, "{binary} is one workspace package");
        let mut seen = std::collections::BTreeSet::new();
        while let Some(id) = stack.pop() {
            let id = id.as_str().expect("package id").to_owned();
            if !seen.insert(id.clone()) {
                continue;
            }
            let node = nodes
                .iter()
                .find(|node| node["id"] == id.as_str())
                .expect("resolved node");
            for dependency in node["deps"].as_array().expect("deps") {
                // Development dependencies never reach the binary.
                if dependency["dep_kinds"]
                    .as_array()
                    .expect("dependency kinds")
                    .iter()
                    .any(|kind| kind["kind"].is_null() || kind["kind"] == "build")
                {
                    stack.push(dependency["pkg"].clone());
                }
            }
        }
        for package in packages
            .iter()
            .filter(|package| seen.contains(package["id"].as_str().expect("id")))
        {
            let manifest = package["manifest_path"].as_str().expect("manifest path");
            let Ok(relative) = Path::new(manifest).strip_prefix(&root) else {
                continue;
            };
            let relative = relative.to_str().expect("path");
            let components = relative.split('/').collect::<Vec<_>>();
            // The last TEST_ONLY entry stands for every apps/<name>/tests/.
            let application_tests =
                components.len() > 2 && components[0] == "apps" && components[2] == "tests";
            let test_only =
                application_tests || TEST_ONLY[..6].iter().any(|path| relative.starts_with(path));
            assert!(
                !test_only,
                "{binary} builds from the test-only path {relative}"
            );
        }
    }
}

/// The second run of one source builds nothing and still gets its own labels.
#[test]
fn a_second_run_of_one_source_relabels_instead_of_building() {
    let directory = TestDirectory::new();
    executable(&directory.path("docker"), FAKE_DOCKER_ABSENT);
    let repository = source_repository(&directory);
    let source = repository.to_str().expect("path");
    let first = stage_identity(&directory, &repository, "host");

    let built = run(
        &directory,
        &[
            "ensure", source, "host", "host", HEAD_LABEL, CLUSTER, CLUSTER,
        ],
    );
    assert!(
        built.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&built.stderr)
    );
    let log = calls(&directory);
    assert!(
        log.contains("--target\thost") && log.contains(&format!("wamn-host:src-{first}")),
        "the first run must build the identity image: {log}"
    );
    assert!(
        log.contains(&format!("wamn-host:{CLUSTER}")),
        "the run must get its own tag: {log}"
    );
    assert!(
        state(&directory, "used", "host").join(&first).exists(),
        "the identity records its last use"
    );
    assert!(
        state(&directory, "leases", "host")
            .join(format!("{first}--{CLUSTER}"))
            .exists(),
        "the run must lease the identity it holds"
    );

    executable(&directory.path("docker"), FAKE_DOCKER_PRESENT);
    fs::write(directory.path("calls"), "").expect("reset the call log");
    let reused = run(
        &directory,
        &[
            "ensure", source, "host", "host", HEAD_LABEL, CLUSTER, CLUSTER,
        ],
    );

    assert!(
        reused.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&reused.stderr)
    );
    let log = calls(&directory);
    assert!(
        !log.contains("--target"),
        "an existing identity must not be rebuilt: {log}"
    );
    assert_eq!(
        log.matches("docker\tbuild").count(),
        1,
        "the only build is the relabel: {log}"
    );
    assert!(
        log.contains(&format!("wamn-host:{CLUSTER}")),
        "the relabel must carry the run tag: {log}"
    );
}

/// Retention keeps the newest three identities and never one a run holds.
#[test]
fn retention_keeps_three_identities_and_never_a_leased_one() {
    let directory = TestDirectory::new();
    executable(&directory.path("docker"), FAKE_DOCKER_PRESENT);
    let used = state(&directory, "used", "host");
    let leases = state(&directory, "leases", "host");
    fs::create_dir_all(&used).expect("create the used directory");
    fs::create_dir_all(&leases).expect("create the lease directory");
    // Oldest first, so the newest three are e, d and c.
    for name in ["a", "b", "c", "d", "e"] {
        fs::write(used.join(name), "").expect("record a use");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    fs::write(leases.join(format!("a--{CLUSTER}")), "").expect("lease the oldest");

    let output = run(&directory, &["retain", "host"]);

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = calls(&directory);
    assert!(
        log.contains("image\trm\twamn-host:src-b"),
        "the oldest unleased identity must go: {log}"
    );
    assert!(
        !log.contains("image\trm\twamn-host:src-a"),
        "a leased identity must never be removed: {log}"
    );
    for kept in ["c", "d", "e"] {
        assert!(
            !log.contains(&format!("image\trm\twamn-host:src-{kept}")),
            "a retained identity was removed: {log}"
        );
    }
    assert!(
        used.join("a").exists(),
        "a leased identity keeps its record"
    );
    assert!(
        !used.join("b").exists(),
        "a removed identity keeps a record"
    );

    let released = run(&directory, &["release", CLUSTER]);
    assert!(released.status.success());
    assert!(
        !leases.join(format!("a--{CLUSTER}")).exists(),
        "release must drop the lease"
    );

    fs::write(directory.path("calls"), "").expect("reset the call log");
    let after = run(&directory, &["retain", "host"]);
    assert!(after.status.success());
    assert!(
        calls(&directory).contains("image\trm\twamn-host:src-a"),
        "the released identity must become removable"
    );
}

/// A prepared context is identified by what it holds, not by what a caller says.
///
/// The WMS debug image is built from a binary this run compiled, so no tree
/// object describes it. A caller that passed its own identity could hand over
/// a stale one and reuse the wrong image without an error (wamn-szr0).
#[test]
fn a_prepared_context_is_identified_by_its_own_content() {
    let directory = TestDirectory::new();
    executable(&directory.path("docker"), FAKE_DOCKER_ABSENT);
    let context = directory.path("host-image");
    fs::create_dir_all(&context).expect("create the context");
    fs::write(context.join("Dockerfile"), "FROM scratch\n").expect("write the context Dockerfile");
    fs::write(context.join("wamn-host"), "one").expect("write the built binary");
    let context = context.to_str().expect("path");

    let built = run(
        &directory,
        &[
            "ensure-context",
            context,
            "host",
            HEAD_LABEL,
            CLUSTER,
            CLUSTER,
            "debug",
        ],
    );

    assert!(
        built.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&built.stderr)
    );
    let first = String::from_utf8_lossy(&built.stdout).trim().to_owned();
    assert!(
        first.starts_with("wamn-host:src-"),
        "the context must name an identity image, got {first}"
    );
    let log = calls(&directory);
    assert!(
        log.contains(&format!("LABEL wamn.dev/source-head=\"{HEAD_LABEL}\"")),
        "the relabel must carry this run's own commit: {log}"
    );
    assert!(
        log.contains("wamn.dev/build-profile=\"debug\""),
        "the caller's build profile must reach the relabel: {log}"
    );

    // The same bytes keep the identity; a changed binary must not reuse it.
    fs::write(directory.path("calls"), "").expect("reset the call log");
    let again = run(
        &directory,
        &[
            "ensure-context",
            context,
            "host",
            HEAD_LABEL,
            CLUSTER,
            CLUSTER,
            "debug",
        ],
    );
    assert_eq!(
        String::from_utf8_lossy(&again.stdout).trim(),
        first,
        "identical content changed the identity"
    );

    fs::write(directory.path("host-image/wamn-host"), "two").expect("rebuild the binary");
    let changed = run(
        &directory,
        &[
            "ensure-context",
            context,
            "host",
            HEAD_LABEL,
            CLUSTER,
            CLUSTER,
            "debug",
        ],
    );
    assert_ne!(
        String::from_utf8_lossy(&changed.stdout).trim(),
        first,
        "a changed binary reused the old identity"
    );
}

/// A context nobody could read must refuse, not produce a confident identity.
///
/// A file listing has two silent failures: an unreadable file that contributes
/// nothing, and an empty directory that hashes to the same value every time.
/// Either one hands back an identity for a context that was never read, and
/// every such context shares it (wamn-szr0).
#[test]
fn an_unreadable_context_is_refused_instead_of_identified() {
    let directory = TestDirectory::new();
    executable(&directory.path("docker"), FAKE_DOCKER_ABSENT);
    let empty = directory.path("empty");
    fs::create_dir_all(&empty).expect("create the empty context");

    let absent = run(
        &directory,
        &[
            "ensure-context",
            directory.path("nowhere").to_str().expect("path"),
            "host",
            HEAD_LABEL,
            CLUSTER,
            CLUSTER,
            "debug",
        ],
    );
    assert_eq!(absent.status.code(), Some(64));
    assert!(
        String::from_utf8_lossy(&absent.stderr).contains("is absent"),
        "the refusal must name the missing context"
    );

    let without_dockerfile = run(
        &directory,
        &[
            "ensure-context",
            empty.to_str().expect("path"),
            "host",
            HEAD_LABEL,
            CLUSTER,
            CLUSTER,
            "debug",
        ],
    );

    assert_eq!(without_dockerfile.status.code(), Some(64));
    assert!(
        String::from_utf8_lossy(&without_dockerfile.stderr).contains("no Dockerfile"),
        "an empty context must refuse rather than hash to nothing"
    );
    assert_eq!(calls(&directory), "", "a refused context reached Docker");
}

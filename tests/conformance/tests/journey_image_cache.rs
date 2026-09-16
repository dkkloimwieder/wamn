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
    for name in ["crates", "apps", "services", "test-support", "tests", "deploy"] {
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
    git(&repository, &["config", "user.email", "test@example.invalid"]);
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
    let output = run(directory, &["identity", repository.to_str().expect("path")]);
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

/// The second run of one source builds nothing and still gets its own labels.
#[test]
fn a_second_run_of_one_source_relabels_instead_of_building() {
    let directory = TestDirectory::new();
    executable(&directory.path("docker"), FAKE_DOCKER_ABSENT);
    let repository = source_repository(&directory);
    let source = repository.to_str().expect("path");
    let first = identity(&directory, &repository);

    let built = run(
        &directory,
        &["ensure", source, "host", "host", HEAD_LABEL, CLUSTER, CLUSTER],
    );
    assert!(
        built.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&built.stderr)
    );
    let log = calls(&directory);
    assert!(
        log.contains(&format!("--target\thost")) && log.contains(&format!("wamn-host:src-{first}")),
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
        &["ensure", source, "host", "host", HEAD_LABEL, CLUSTER, CLUSTER],
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
    assert!(used.join("a").exists(), "a leased identity keeps its record");
    assert!(!used.join("b").exists(), "a removed identity keeps a record");

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

//! Refuse unreadable contexts and preserve leased images over a fake Docker.

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

fn calls(directory: &TestDirectory) -> String {
    fs::read_to_string(directory.path("calls")).unwrap_or_default()
}

fn state(directory: &TestDirectory, kind: &str, name: &str) -> PathBuf {
    directory.path(&format!("cache/wamn-journey-images/{kind}/{name}"))
}

/// Retention preserves images that a run still holds.
#[test]
fn retention_never_removes_a_leased_identity() {
    let directory = TestDirectory::new();
    executable(&directory.path("docker"), FAKE_DOCKER_PRESENT);
    let used = state(&directory, "used", "host");
    let leases = state(&directory, "leases", "host");
    fs::create_dir_all(&used).expect("create the used directory");
    fs::create_dir_all(&leases).expect("create the lease directory");
    // Oldest first, so the newest three are e, d and c.
    for name in ["a", "b", "c", "d", "e"] {
        fs::write(used.join(name), "").expect("record a use");
        let file = fs::File::options()
            .write(true)
            .open(used.join(name))
            .unwrap();
        file.set_modified(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::from(name.as_bytes()[0])),
        )
        .unwrap();
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

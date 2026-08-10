//! Guards the current Wasmtime alignment documentation against dependency drift.

use std::fs;
use std::path::{Path, PathBuf};

const TYPE_UNIVERSE_GATE: &str =
    "cargo test -p wamn-proof-conformance --test wasmtime_source_identity";
const TYPE_UNIVERSE_GATE_PATH: &str = "tests/conformance/tests/wasmtime_source_identity.rs";
const WASMTIME_VERSION: &str = "47.0.1";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source
        .find(start)
        .unwrap_or_else(|| panic!("missing section start `{start}`"));
    let end = source[start..]
        .find(end)
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("missing section end `{end}`"));
    &source[start..end]
}

#[test]
fn current_wasmtime_documentation_matches_executable_type_universe_contract() {
    let root = repository_root();
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("read root Cargo.toml");
    let dependency_preamble = section(&manifest, "[workspace.dependencies]", "wash-runtime = ");

    assert!(
        dependency_preamble.contains("one crates.io Wasmtime 47.0.1 family"),
        "wash-runtime dependency comment must state the current Wasmtime family"
    );
    assert!(
        dependency_preamble.contains(TYPE_UNIVERSE_GATE_PATH),
        "wash-runtime dependency comment must point to the executable type-universe gate"
    );
    assert!(
        !dependency_preamble.contains("git-pinned"),
        "wash-runtime dependency comment must not describe the retired git-pinned universe"
    );
    for dependency in ["wasmtime-wasi", "wasmtime-wasi-http"] {
        assert!(
            manifest.contains(&format!("{dependency} = \"{WASMTIME_VERSION}\"")),
            "workspace must pin {dependency} to Wasmtime {WASMTIME_VERSION}"
        );
    }

    let fork_doc = fs::read_to_string(root.join("docs/archive/platform/wash-runtime-fork.md"))
        .expect("read wash-runtime fork documentation");
    let current_posture = section(
        &fork_doc,
        "# Consuming `wash-runtime` from the wamn fork",
        "## Carried commits",
    );
    assert!(
        current_posture.contains("one crates.io\n  Wasmtime 47.0.1 family"),
        "current fork posture must state the crates.io Wasmtime 47.0.1 family"
    );
    assert!(
        current_posture.contains(TYPE_UNIVERSE_GATE),
        "current fork posture must point to the executable type-universe gate"
    );
    for stale in ["git-pinned one", "wasmtime 46.0.0", "7535c0255b2b84f6"] {
        assert!(
            !current_posture.contains(stale),
            "current fork posture retains stale Wasmtime wording `{stale}`"
        );
    }

    let sync_runbook = section(&fork_doc, "## Sync runbook", "## Sync log");
    assert!(
        sync_runbook.contains("crates.io `wasmtime-wasi` and `wasmtime-wasi-http`"),
        "sync runbook must align the crates.io Wasmtime family"
    );
    assert!(
        sync_runbook.contains(TYPE_UNIVERSE_GATE),
        "sync runbook must run the executable type-universe gate"
    );

    let sync_log = fork_doc
        .split_once("## Sync log")
        .map(|(_, log)| log)
        .expect("fork documentation must retain its sync log");
    assert!(
        sync_log.contains("`wamn/2.5.2`"),
        "historical sync-log rows must remain historical"
    );
}

//! Guards the build-environment pointer to the wash-runtime fork ledger.

use std::fs;
use std::path::{Path, PathBuf};

const BUILD_AND_TEST_DOC: &str = "docs/build-and-test.md";
const FORK_LEDGER: &str = "docs/wash-runtime-fork.md";
const ROOT_MANIFEST: &str = "Cargo.toml";

const EXPECTED_BUILD_ENVIRONMENT_PREAMBLE: &str = r#"wamn-host builds against wash-runtime consumed as a **git dependency from our
fork** (dkkloimwieder/wasmCloud, branch `wamn/2.6.0` = upstream v2.6.0).
`docs/wash-runtime-fork.md` is the authoritative carried-policy ledger and
rev-bump runbook; this preamble does not duplicate its commit or seam
inventory. The rev is pinned in one place:
`workspace.dependencies.wash-runtime.rev` in the root `Cargo.toml`."#;

const EXPECTED_MANIFEST_LEDGER_COMMENT: &str = "# Upstream v2.6.0 plus the policies recorded in\n\
# docs/wash-runtime-fork.md. The ledger is authoritative.";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn read_repository_file(root: &Path, relative: &str) -> String {
    let path = root.join(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

fn build_environment_preamble(document: &str) -> &str {
    document
        .split_once("## Build environment\n")
        .expect("build-and-test documentation must contain the Build environment section")
        .1
        .split_once("\n### ")
        .expect("Build environment preamble must end before its first subsection")
        .0
        .trim()
}

fn wash_runtime_manifest_context(manifest: &str) -> (&str, &str) {
    let workspace_dependencies = manifest
        .split_once("[workspace.dependencies]\n")
        .expect("root manifest must contain workspace dependencies")
        .1;
    let pin_start = workspace_dependencies
        .find("wash-runtime = {")
        .expect("workspace dependencies must contain the wash-runtime pin");
    let before_pin = workspace_dependencies[..pin_start].trim_end();
    let pin = workspace_dependencies[pin_start..]
        .lines()
        .next()
        .expect("wash-runtime pin must occupy a manifest line");

    (before_pin, pin)
}

#[test]
fn build_environment_preamble_tracks_current_fork_without_copying_policy_inventory() {
    let root = repository_root();
    let build_and_test = read_repository_file(&root, BUILD_AND_TEST_DOC);
    let manifest = read_repository_file(&root, ROOT_MANIFEST);
    let ledger = read_repository_file(&root, FORK_LEDGER);

    assert_eq!(
        build_environment_preamble(&build_and_test),
        EXPECTED_BUILD_ENVIRONMENT_PREAMBLE,
        "Build environment preamble must name the current fork and delegate policy details"
    );

    let (manifest_comment_context, pin) = wash_runtime_manifest_context(&manifest);
    assert!(
        manifest_comment_context.contains(EXPECTED_MANIFEST_LEDGER_COMMENT),
        "root wash-runtime pin must identify the ledger as authoritative"
    );
    assert!(
        pin.contains("git = \"https://github.com/dkkloimwieder/wasmCloud\"")
            && pin.contains("rev = \"")
            && !pin.contains("branch = "),
        "root wash-runtime dependency must pin an immutable revision from the wamn fork"
    );

    assert!(
        ledger.contains("Current: `wamn/2.6.0` = upstream v2.6.0")
            && ledger.contains("## Carried commits (the ledger)")
            && ledger.contains("## Sync runbook"),
        "referenced fork document must record the current base, policy ledger, and rev-bump runbook"
    );
}

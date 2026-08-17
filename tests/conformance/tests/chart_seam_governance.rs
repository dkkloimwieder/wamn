//! Guards the runtime-operator chart seam ruling 4's manifest mount rides.
//!
//! The chart is pulled from OCI at install time and is not in this repository,
//! so no hermetic test can render it. What is guarded instead is the coupling
//! that makes a chart move visible: `deploy/infra/values-wamn.yaml` records the
//! seam the mount depends on against the fork revision whose chart was
//! inspected, and that revision is the pin in the root `Cargo.toml`. Moving the
//! pin without re-inspecting the chart fails here (wamn-0h0g.15.54).

use std::fs;
use std::path::{Path, PathBuf};

const CARGO_MANIFEST: &str = "Cargo.toml";
const FORK_LEDGER: &str = "docs/archive/platform/wash-runtime-fork.md";
const VALUES: &str = "deploy/infra/values-wamn.yaml";

const RE_VERIFY: &str = "grep -n 'with \\.volumes\\|with \\.volumeMounts'";

/// The values key the manifest mount sets, paired with the chart template
/// expression that renders it — the two halves a rename would separate.
const SEAM: [(&str, &str); 2] = [
    ("runtime.hostGroups[].volumes", "{{- with .volumes }}"),
    ("runtime.hostGroups[].volumeMounts", "{{- with .volumeMounts }}"),
];

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

/// The pin is the single source of truth for which chart the install runs
/// against: the chart ships inside the fork tree at this revision.
fn pinned_fork_revision(manifest: &str) -> &str {
    let pin = manifest
        .lines()
        .find(|line| line.starts_with("wash-runtime = {"))
        .expect("root manifest must pin wash-runtime");
    let revision = pin
        .split_once("rev = \"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(revision, _)| revision)
        .expect("wash-runtime pin must carry a quoted rev");
    assert_eq!(
        revision.len(),
        40,
        "wash-runtime must pin a full 40-character revision, found {revision:?}"
    );

    revision
}

/// `wamn/X.Y.Z` is the peeled upstream `vX.Y.Z` tag plus carried commits, and
/// upstream's release train packages `charts/runtime-operator` at that same
/// version — so the ledger's current branch names the chart version the pinned
/// revision carries.
fn ledger_fork_version(ledger: &str) -> &str {
    ledger
        .split_once("Current: `wamn/")
        .and_then(|(_, rest)| rest.split_once('`'))
        .map(|(version, _)| version)
        .expect("fork ledger must name the current `wamn/X.Y.Z` branch")
}

fn installed_chart_version(values: &str) -> &str {
    values
        .split_once("--version ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .expect("values file must document the chart version its install pulls")
}

#[test]
fn seam_record_tracks_the_pinned_fork_revision() {
    let root = repository_root();
    let manifest = read_repository_file(&root, CARGO_MANIFEST);
    let ledger = read_repository_file(&root, FORK_LEDGER);
    let values = read_repository_file(&root, VALUES);

    let revision = pinned_fork_revision(&manifest);
    let expected = format!(
        "Pinned: fork rev {} = chart {}.",
        &revision[..8],
        ledger_fork_version(&ledger)
    );

    assert!(
        values.contains(&expected),
        "{VALUES} records a seam verified at a different chart than the pinned \
         fork revision carries. Re-inspect the chart at the new pin \
         (`{RE_VERIFY} …`), then record `{expected}`"
    );
}

#[test]
fn seam_record_names_both_undeclared_passthrough_keys() {
    let root = repository_root();
    let values = read_repository_file(&root, VALUES);

    for (key, expression) in SEAM {
        assert!(
            values.contains(key),
            "{VALUES} must record the values key {key:?} that reaches the seam"
        );
        assert!(
            values.contains(expression),
            "{VALUES} must record the chart template expression {expression:?} \
             that {key} renders through"
        );
    }
    assert!(
        values.contains("e256a9f6"),
        "{VALUES} must record the upstream commit that introduced the passthrough keys"
    );
    assert!(
        values.contains("no values.schema.json"),
        "{VALUES} must record why a rename is silent rather than an install error"
    );
    assert!(
        values.contains(RE_VERIFY),
        "{VALUES} must record the command that re-verifies the seam at a new pin"
    );
}

#[test]
fn install_command_and_seam_record_agree_on_the_installed_chart() {
    let root = repository_root();
    let values = read_repository_file(&root, VALUES);

    let installed = installed_chart_version(&values);
    let expected = format!("pulls chart {installed},");

    assert!(
        values.contains(&expected),
        "{VALUES} install command pulls chart {installed}, which its seam record \
         does not state; expected {expected:?}"
    );
}

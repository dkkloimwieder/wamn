use serde::Deserialize;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

const PACKAGE_ROLES: &str = "architecture/package-roles.json";
const SCOPE_REDUCTION: &str = "docs/scope-reduction-mvp.md";
const OUTCOME_PREFIX: &str = "//! MVP outcome: ";

const CRASH_FLOOR: &str = "crash floor · M0 execution · flow composition";
const AUTHENTICATED_ADMISSION: &str = "M0 authenticated admission via the warm run-worker";
const EVENT_SPINE: &str = "event spine (causation depth = loop guard)";
const WAKE_FROM_ZERO: &str = "wake-from-zero";
const PUBLISH_GATE: &str = "publish gate";
const PROVISIONING: &str =
    "provisioning · publish · additive schema · tenant isolation (T1 minting)";
const MANAGEMENT_AUTH: &str = "management auth";
const POSTGRES_NODE: &str = "the Postgres standard node (`standard-nodes/src/postgres.rs`)";
const EGRESS_CONFINEMENT: &str = "egress confinement (import allowlist, mutation-proofed)";
const M0_NODE_SET: &str = "M0 node set";
const PROOF_FLOOR: &str = "proof floor";

// (package, root module, F.1 outcome)
const RETAINED_ROOTS: &[(&str, &str, &str)] = &[
    (
        "wamn-authoring-model",
        "crates/authoring/model/src/lib.rs",
        PUBLISH_GATE,
    ),
    (
        "wamn-catalog",
        "crates/catalog/model/src/lib.rs",
        PROVISIONING,
    ),
    (
        "wamn-cdc-reader",
        "services/cdc-reader/src/lib.rs",
        EVENT_SPINE,
    ),
    (
        "wamn-component-policy",
        "crates/platform/component-policy/src/lib.rs",
        EGRESS_CONFINEMENT,
    ),
    (
        "wamn-control-provision",
        "crates/control/provision/src/lib.rs",
        PROVISIONING,
    ),
    (
        "wamn-control-registry",
        "crates/control/registry/src/lib.rs",
        PROVISIONING,
    ),
    ("wamn-ctl", "services/ctl/src/lib.rs", PROVISIONING),
    (
        "wamn-dispatcher",
        "services/dispatcher/src/lib.rs",
        WAKE_FROM_ZERO,
    ),
    (
        "wamn-entity-access",
        "crates/data/entity-access/src/lib.rs",
        POSTGRES_NODE,
    ),
    (
        "wamn-event-reg",
        "crates/events/registration/src/lib.rs",
        EVENT_SPINE,
    ),
    (
        "wamn-event-wire",
        "crates/events/wire/src/lib.rs",
        EVENT_SPINE,
    ),
    (
        "wamn-execution-host",
        "crates/execution/host/src/lib.rs",
        CRASH_FLOOR,
    ),
    ("wamn-executor", "services/executor/src/lib.rs", CRASH_FLOOR),
    (
        "wamn-flow",
        "crates/execution/flow-model/src/lib.rs",
        CRASH_FLOOR,
    ),
    (
        "wamn-flow-invocation",
        "crates/execution/flow-invocation/src/lib.rs",
        AUTHENTICATED_ADMISSION,
    ),
    (
        "wamn-gate-harness",
        "test-support/harness/src/lib.rs",
        PROOF_FLOOR,
    ),
    ("wamn-gates", "tests/orchestrator/src/main.rs", PROOF_FLOOR),
    ("wamn-host", "services/host/src/main.rs", CRASH_FLOOR),
    (
        "wamn-materializer",
        "crates/events/materializer/src/lib.rs",
        EVENT_SPINE,
    ),
    (
        "wamn-pg-core",
        "crates/platform/pg-core/src/lib.rs",
        EVENT_SPINE,
    ),
    (
        "wamn-platform-identity",
        "crates/identity/platform/src/lib.rs",
        MANAGEMENT_AUTH,
    ),
    (
        "wamn-project-state",
        "crates/identity/project-state/src/lib.rs",
        PROVISIONING,
    ),
    (
        "wamn-proof-conformance",
        "tests/conformance/src/lib.rs",
        PROOF_FLOOR,
    ),
    (
        "wamn-proof-integration",
        "tests/integration/src/lib.rs",
        PROOF_FLOOR,
    ),
    ("wamn-proof-system", "tests/system/src/lib.rs", PROOF_FLOOR),
    (
        "wamn-router",
        "crates/execution/router/src/lib.rs",
        CRASH_FLOOR,
    ),
    (
        "wamn-run-state",
        "crates/execution/run-state/src/lib.rs",
        CRASH_FLOOR,
    ),
    (
        "wamn-runner",
        "crates/execution/flow-engine/src/lib.rs",
        CRASH_FLOOR,
    ),
    (
        "wamn-runtime",
        "crates/platform/runtime/src/lib.rs",
        CRASH_FLOOR,
    ),
    (
        "wamn-scenario-model",
        "crates/scenarios/model/src/lib.rs",
        PUBLISH_GATE,
    ),
    (
        "wamn-scenario-worker",
        "services/scenario-worker/src/lib.rs",
        PUBLISH_GATE,
    ),
    (
        "wamn-scheduler",
        "crates/execution/scheduler/src/lib.rs",
        WAKE_FROM_ZERO,
    ),
    (
        "wamn-schema-compiler",
        "crates/schema/compiler/src/lib.rs",
        PROVISIONING,
    ),
    (
        "wamn-schema-control",
        "crates/schema/control/src/lib.rs",
        PROVISIONING,
    ),
    (
        "wamn-schema-model",
        "crates/schema/model/src/lib.rs",
        PROVISIONING,
    ),
    (
        "wamn-standard-nodes",
        "crates/execution/standard-nodes/src/lib.rs",
        M0_NODE_SET,
    ),
    (
        "wamn-test-fixtures",
        "test-support/fixtures/lib.rs",
        PROOF_FLOOR,
    ),
    (
        "wamn-test-infrastructure",
        "test-support/infrastructure/lib.rs",
        PROOF_FLOOR,
    ),
    ("wamn-waker", "services/waker/src/lib.rs", WAKE_FROM_ZERO),
    (
        "flow-http",
        "components/ingress/flow-http/src/lib.rs",
        AUTHENTICATED_ADMISSION,
    ),
    (
        "flowrunner",
        "components/execution/flowrunner/src/lib.rs",
        CRASH_FLOOR,
    ),
    (
        "materializer",
        "components/execution/materializer/src/main.rs",
        EVENT_SPINE,
    ),
    (
        "busyloop",
        "components/fixtures/busyloop/src/main.rs",
        PROOF_FLOOR,
    ),
    (
        "connection-http-standard",
        "components/fixtures/connection-http-standard/src/lib.rs",
        PROOF_FLOOR,
    ),
    (
        "sockprobe",
        "components/fixtures/sockprobe/src/main.rs",
        PROOF_FLOOR,
    ),
];

#[derive(Debug, Deserialize)]
struct PackageRoles {
    packages: Vec<PackageRole>,
}

#[derive(Debug, Deserialize)]
struct PackageRole {
    workspace: String,
    name: String,
    manifest_path: String,
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn read(root: &Path, path: &str) -> String {
    let absolute = root.join(path);
    fs::read_to_string(&absolute)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", absolute.display()))
}

fn manifest_path(module: &str) -> String {
    let parent = Path::new(module)
        .parent()
        .expect("root module must have a parent directory");
    let package = if parent.file_name() == Some(OsStr::new("src")) {
        parent
            .parent()
            .expect("src root must have a package directory")
    } else {
        parent
    };
    package.join("Cargo.toml").to_string_lossy().into_owned()
}

fn workspace(module: &str) -> &'static str {
    if module.starts_with("components/") {
        "components"
    } else {
        "root"
    }
}

fn f1_outcomes(scope_reduction: &str) -> BTreeSet<&str> {
    let ledger = scope_reduction
        .split_once("### F.1 Retained roots → named outcome")
        .expect("scope reduction must retain the F.1 ledger")
        .1
        .split_once("### F.2 Deletion inventory")
        .expect("F.1 must end at the F.2 ledger")
        .0;
    ledger
        .lines()
        .filter_map(|line| line.split('|').nth(2))
        .map(str::trim)
        .filter(|cell| !cell.is_empty() && *cell != "Outcome" && !cell.starts_with("---"))
        .collect()
}

#[test]
fn retained_root_map_exactly_covers_the_package_inventory() {
    let root = repository_root();
    let roles: PackageRoles = serde_json::from_str(&read(&root, PACKAGE_ROLES))
        .expect("package-role inventory must parse");
    let actual = roles
        .packages
        .into_iter()
        .map(|package| (package.workspace, package.name, package.manifest_path))
        .collect::<BTreeSet<_>>();
    let expected = RETAINED_ROOTS
        .iter()
        .map(|&(package, module, _)| {
            (
                workspace(module).to_owned(),
                package.to_owned(),
                manifest_path(module),
            )
        })
        .collect::<BTreeSet<_>>();

    assert_eq!(actual, expected, "retained-root outcome coverage drifted");
    assert_eq!(
        expected.len(),
        RETAINED_ROOTS.len(),
        "duplicate package root"
    );
}

#[test]
fn every_retained_root_names_its_exact_f1_outcome() {
    let root = repository_root();
    let scope_reduction = read(&root, SCOPE_REDUCTION);
    let ledger_outcomes = f1_outcomes(&scope_reduction);
    let mut modules = BTreeSet::new();

    for &(package, module, outcome) in RETAINED_ROOTS {
        assert!(
            ledger_outcomes.contains(outcome),
            "{package} names an outcome absent from F.1: {outcome:?}"
        );
        assert!(modules.insert(module), "duplicate module root");

        let source = read(&root, module);
        let expected = format!("{OUTCOME_PREFIX}{outcome}.");
        let module_markers = source
            .lines()
            .take_while(|line| line.starts_with("//!"))
            .filter(|line| line.starts_with(OUTCOME_PREFIX))
            .collect::<Vec<_>>();
        let all_markers = source
            .lines()
            .filter(|line| line.starts_with(OUTCOME_PREFIX))
            .collect::<Vec<_>>();

        assert_eq!(
            module_markers,
            [expected.as_str()],
            "{module} must name exactly one exact outcome in its root module docs"
        );
        assert_eq!(
            all_markers, module_markers,
            "{module} carries an outcome marker outside its root module docs"
        );
    }
}

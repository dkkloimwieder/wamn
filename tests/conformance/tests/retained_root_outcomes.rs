use serde::Deserialize;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

const PACKAGE_ROLES: &str = "architecture/package-roles.json";

// (package, root module, F.1 outcome)
// (package, root module)
const RETAINED_ROOTS: &[(&str, &str)] = &[
    ("wamn-authoring-model", "crates/authoring/model/src/lib.rs"),
    ("wamn-catalog", "crates/catalog/model/src/lib.rs"),
    ("wamn-client", "crates/client/core/src/lib.rs"),
    ("wamn-client-tui", "crates/client/tui/src/lib.rs"),
    ("wamn-cdc-reader", "services/cdc-reader/src/lib.rs"),
    (
        "wamn-component-policy",
        "crates/platform/component-policy/src/lib.rs",
    ),
    (
        "wamn-component-virtualizer",
        "crates/platform/component-virtualizer/src/main.rs",
    ),
    (
        "wamn-control-provision",
        "crates/control/provision/src/lib.rs",
    ),
    (
        "wamn-control-registry",
        "crates/control/registry/src/lib.rs",
    ),
    ("wamn-ctl", "services/ctl/src/lib.rs"),
    ("wamn-dispatcher", "services/dispatcher/src/lib.rs"),
    ("wamn-event-reg", "crates/events/registration/src/lib.rs"),
    ("wamn-event-wire", "crates/events/wire/src/lib.rs"),
    (
        "wamn-execution-contract",
        "crates/execution/contract/src/lib.rs",
    ),
    ("wamn-execution-host", "crates/execution/host/src/lib.rs"),
    ("wamn-executor", "services/executor/src/lib.rs"),
    ("wamn-gate-harness", "test-support/harness/src/lib.rs"),
    ("wamn-gates", "tests/orchestrator/src/main.rs"),
    ("wamn-host", "services/host/src/main.rs"),
    ("wamn-materializer", "crates/events/materializer/src/lib.rs"),
    ("wamn-pg-core", "crates/platform/pg-core/src/lib.rs"),
    (
        "wamn-platform-identity",
        "crates/identity/platform/src/lib.rs",
    ),
    (
        "wamn-project-state",
        "crates/identity/project-state/src/lib.rs",
    ),
    ("wamn-proof-conformance", "tests/conformance/src/lib.rs"),
    ("wamn-proof-integration", "tests/integration/src/lib.rs"),
    ("wamn-proof-system", "tests/system/src/lib.rs"),
    ("wamn-router", "crates/execution/router/src/lib.rs"),
    ("wamn-run-state", "crates/execution/run-state/src/lib.rs"),
    ("wamn-runtime", "crates/platform/runtime/src/lib.rs"),
    ("wamn-scenario-model", "crates/scenarios/model/src/lib.rs"),
    (
        "wamn-scenario-worker",
        "services/scenario-worker/src/lib.rs",
    ),
    ("wamn-scheduler", "crates/execution/scheduler/src/lib.rs"),
    ("wamn-schema-control", "crates/schema/control/src/lib.rs"),
    (
        "wamn-schema-generator",
        "crates/schema/generator/src/lib.rs",
    ),
    (
        "wamn-schema-introspection",
        "crates/schema/introspection/src/lib.rs",
    ),
    ("wamn-simulator", "test-support/simulator/src/lib.rs"),
    (
        "wamn-test-infrastructure",
        "test-support/infrastructure/lib.rs",
    ),
    ("wamn-waker", "services/waker/src/lib.rs"),
    ("http-route", "components/ingress/http-route/src/lib.rs"),
    (
        "materializer",
        "components/execution/materializer/src/main.rs",
    ),
    ("blob-put", "components/execution/blob-put/src/lib.rs"),
    (
        "wamn-postgres-statements",
        "components/data/postgres-statements/src/lib.rs",
    ),
    (
        "wamn-postgres-sqlx",
        "components/data/postgres-sqlx/src/lib.rs",
    ),
    (
        "wamn-receiving-data-access",
        "components/data/receiving-data/src/lib.rs",
    ),
    ("receiving", "components/application/receiving/src/lib.rs"),
    (
        "client-acme-receiving",
        "components/application/client-acme-receiving/src/lib.rs",
    ),
    (
        "wamn-client-acme-receiving-data-access",
        "components/data/client-acme-receiving-data/src/lib.rs",
    ),
    ("busyloop", "components/fixtures/busyloop/src/main.rs"),
    (
        "connection-http-standard",
        "components/fixtures/connection-http-standard/src/lib.rs",
    ),
    ("sockprobe", "components/fixtures/sockprobe/src/main.rs"),
    (
        "sqlx-command",
        "components/fixtures/sqlx-command/src/main.rs",
    ),
    (
        "std-virtualization-probe",
        "components/fixtures/std-virtualization-probe/src/lib.rs",
    ),
    ("http-request", "components/no-std/http-request/src/lib.rs"),
    ("label-render", "components/no-std/label-render/src/lib.rs"),
    (
        "label-template",
        "components/no-std/label-template/src/lib.rs",
    ),
    ("transform", "components/no-std/transform/src/lib.rs"),
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
    // The guests live in more than one Cargo workspace: feature unification is
    // additive-only, so the `no_std` palette guests are isolated from the
    // members that reach serde_json/std (wamn-0h0g.11.56).
    if module.starts_with("components/no-std/") {
        "components-no-std"
    } else if module.starts_with("components/") {
        "components"
    } else {
        "root"
    }
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
        .map(|&(package, module)| {
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

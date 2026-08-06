//! Guards the build-environment pointer to the wash-runtime fork ledger.

use std::fs;
use std::path::{Path, PathBuf};

const ACTIVE_PLAN: &str = "docs/PLAN/PLAN.md";
const BUILD_AND_TEST_DOC: &str = "docs/build-and-test.md";
const CARGO_LOCK: &str = "Cargo.lock";
const FORK_LEDGER: &str = "docs/platform/wash-runtime-fork.md";
const ROOT_MANIFEST: &str = "Cargo.toml";
const RUST_TOOLCHAIN: &str = "rust-toolchain.toml";
const UPGRADE_DELTA: &str = "docs/PLAN/WASMCLOUD-UPGRADE-2.6.1.md";

const EXPECTED_BUILD_ENVIRONMENT_PREAMBLE: &str = r#"wamn-host builds against wash-runtime consumed as a **git dependency from our
fork** (dkkloimwieder/wasmCloud, branch `wamn/2.6.1` = upstream v2.6.1).
`docs/platform/wash-runtime-fork.md` is the authoritative carried-policy ledger and
rev-bump runbook; this preamble does not duplicate its commit or seam
inventory. The rev is pinned in one place:
`workspace.dependencies.wash-runtime.rev` in the root `Cargo.toml`."#;
const EXPECTED_MANIFEST_LEDGER_COMMENT: &str = "# Upstream v2.6.1 plus the policies recorded in\n\
# docs/platform/wash-runtime-fork.md. The ledger is authoritative.";
const EXPECTED_PLAN_REVISION: &str = "09b1132f";
const EXPECTED_REVISION: &str = "09b1132f2bab36e6e71f4637bd0e4755e359dd43";
const EXPECTED_UPSTREAM_BASE: &str = "df8a8bcd69adc9c23ded842e504071a5272d04ed";
const POLICY_COMMITS: [&str; 7] = [
    "f90d977f", "24b220f5", "6ca3d6f7", "0d98f850", "a9f9c57d", "95b04ded", "33b24183",
];
const EXIT_CONDITIONS: [&str; 7] = [
    "upstream ships native epoch-deadline support — delete the commit (the wamn-host ticker/config side stays as-is)",
    "upstream plumbs `memory_limit_mb` into a Store limiter — delete the commit",
    "upstream provides equivalent host-enforced P2/P3 trace-context injection across HTTP, gRPC, and custom transports, including client-span parenting — delete the commit",
    "upstream gates socket linking on `host_interfaces`, or consults an egress policy for `TcpConnect` — delete the commit",
    "upstream gates socket linking on `host_interfaces`, or consults an egress policy for UDP connect/datagram operations — delete the commit",
    "upstream exposes equivalent limiter introspection accessors — delete the commit",
    "upstream provides an equivalent inbound request-count metric, and the wamn dashboards, SLOs, and mutation gate have migrated to it and passed — delete the commit",
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

fn section<'a>(document: &'a str, start: &str, end: &str) -> &'a str {
    let (_, remainder) = document
        .split_once(start)
        .unwrap_or_else(|| panic!("missing section start {start:?}"));
    remainder
        .split_once(end)
        .map_or(remainder, |(contents, _)| contents)
}

fn build_environment_preamble(document: &str) -> &str {
    section(document, "## Build environment\n", "\n### ").trim()
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

fn wash_runtime_lock_package(lockfile: &str) -> &str {
    let packages = lockfile
        .split("[[package]]")
        .filter(|package| {
            package
                .lines()
                .any(|line| line == "name = \"wash-runtime\"")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        packages.len(),
        1,
        "Cargo.lock must resolve exactly one wash-runtime package"
    );
    packages[0]
}

#[test]
fn build_environment_preamble_tracks_exact_current_fork() {
    let root = repository_root();
    let build_and_test = read_repository_file(&root, BUILD_AND_TEST_DOC);
    let manifest = read_repository_file(&root, ROOT_MANIFEST);
    let lockfile = read_repository_file(&root, CARGO_LOCK);
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
    assert_eq!(
        pin,
        format!(
            "wash-runtime = {{ git = \"https://github.com/dkkloimwieder/wasmCloud\", rev = \"{EXPECTED_REVISION}\", default-features = false }}"
        ),
        "root wash-runtime dependency must pin the exact immutable fork revision"
    );

    let lock_package = wash_runtime_lock_package(&lockfile);
    assert!(
        lock_package.contains("version = \"2.6.1\"")
            && lock_package.contains(&format!(
                "source = \"git+https://github.com/dkkloimwieder/wasmCloud?rev={EXPECTED_REVISION}#{EXPECTED_REVISION}\""
            )),
        "Cargo.lock must resolve wash-runtime 2.6.1 from the exact fork revision"
    );

    assert!(
        ledger.contains(&format!(
            "Current: `wamn/2.6.1` = upstream v2.6.1\n  (`{EXPECTED_UPSTREAM_BASE}`)"
        )) && ledger.contains(&format!("final fork tip `{EXPECTED_REVISION}`"))
            && ledger.contains("## Carried commits (the ledger)")
            && ledger.contains("## Sync runbook"),
        "referenced fork document must record the current base, tip, policy ledger, and runbook"
    );
}

#[test]
fn fork_ledger_records_exact_seven_policy_commits_and_exit_conditions() {
    let root = repository_root();
    let ledger = read_repository_file(&root, FORK_LEDGER);
    let carried = section(
        &ledger,
        "## Carried commits (the ledger)",
        "Everything else epoch-related",
    );
    let rows = carried
        .lines()
        .filter(|line| line.starts_with("| `"))
        .collect::<Vec<_>>();
    let commits = rows
        .iter()
        .map(|row| {
            row.split('`')
                .nth(1)
                .expect("carried-policy row must begin with a commit")
        })
        .collect::<Vec<_>>();

    assert_eq!(
        commits, POLICY_COMMITS,
        "ledger must contain the exact seven carried policy commits in re-port order"
    );
    for (row, exit_condition) in rows.iter().zip(EXIT_CONDITIONS) {
        assert!(
            row.contains(exit_condition),
            "carried-policy exit condition drifted: {exit_condition}"
        );
    }
    for proof_fix in ["f9fcf287", "09b1132f"] {
        assert!(
            !commits.contains(&proof_fix),
            "proof/restoration fix {proof_fix} must not become a carried-policy row"
        );
    }
    for required in [
        "the `Ingress` P2 and P3 host surfaces",
        "`AllowedIPNameLookups`",
    ] {
        assert!(
            carried.contains(required),
            "current carried-policy ledger lost canonical v2.6.1 name {required:?}"
        );
    }
    for stale in [
        "the `HttpServer` P2 and P3 host surfaces",
        "`allowIpNameLookup`",
    ] {
        assert!(
            !carried.contains(stale),
            "current carried-policy ledger restored obsolete name {stale:?}"
        );
    }
}

#[test]
fn upstream_delta_and_dependency_invariants_are_exact() {
    let root = repository_root();
    let ledger = read_repository_file(&root, FORK_LEDGER);
    let manifest = read_repository_file(&root, ROOT_MANIFEST);
    let rust_toolchain = read_repository_file(&root, RUST_TOOLCHAIN);
    let upgrade_delta = read_repository_file(&root, UPGRADE_DELTA);
    let delta = section(
        &ledger,
        "## Current upstream delta (v2.6.0 → v2.6.1)",
        "## Carried commits",
    );

    for required in [
        "git ls-remote --tags",
        "001067539ac8638b8987d10e34c2c247a1798138",
        EXPECTED_UPSTREAM_BASE,
        "dd8eccedd9eb96458fa6443fb940ba4bfb57667a",
        "9c7aa1fa9dbe6ab243797a655e74b6049aaa8193",
        "`HttpServer` to `Ingress`",
        "`HttpServerBuilder` to `IngressBuilder`",
        "`AllowedIPNameLookups`",
        "`allowedIpNameLookups`",
        "`allowed_ip_name_lookups`",
        "Wasmtime family at 47.0.1",
        "`async-nats` 0.49.1",
        "`rust-version` 1.94.0",
        "All seven carried-policy exit conditions remain unchanged and unsatisfied",
    ] {
        assert!(
            delta.contains(required),
            "current upstream delta lost required evidence {required:?}"
        );
    }

    let current_sync = ledger
        .lines()
        .find(|line| line.starts_with("| 2026-07-31 |"))
        .expect("sync log must contain the v2.6.1 retarget record");
    for required in [
        "`dd8ecced`",
        "`9c7aa1fa`",
        "`df8a8bcd`",
        "Dependencies are unchanged",
        "all seven exit conditions are unchanged",
        "wamn-g2br.11",
    ] {
        assert!(
            current_sync.contains(required),
            "v2.6.1 sync record lost required evidence {required:?}"
        );
    }

    assert!(
        upgrade_delta.contains("'refs/tags/v2.6.1' 'refs/tags/v2.6.1^{}'")
            && !upgrade_delta.contains("refs/tags/runtime-operator/v2.6.1"),
        "upgrade delta must verify the real v2.6.1 tag and peeled ref"
    );
    assert!(
        manifest.contains("wasmtime-wasi = \"47.0.1\"")
            && manifest.contains("wasmtime-wasi-http = \"47.0.1\"")
            && manifest.contains("async-nats = { version = \"0.49.1\"")
            && rust_toolchain.contains("upstream wasmCloud's rust-version 1.94.0"),
        "workspace dependency and upstream MSRV documentation must match the unchanged delta"
    );
}

#[test]
fn active_plan_points_to_the_v2_6_1_delta() {
    let root = repository_root();
    let plan = read_repository_file(&root, ACTIVE_PLAN);

    assert!(
        plan.contains("**The v2.6.1 upgrade is complete.**")
            && plan.contains("as `wamn/2.6.1`, pinned at rev")
            && plan.contains(&format!("`{EXPECTED_PLAN_REVISION}`"))
            && plan.contains("`docs/PLAN/WASMCLOUD-UPGRADE-2.6.1.md`")
            && plan.contains("`docs/PLAN/WASMCLOUD-UPGRADE-2.6.0.md`")
            && plan.contains("It was a **policy re-port**, not a dependency bump"),
        "active roadmap must record the completed fork retarget and retained upgrade records"
    );
}

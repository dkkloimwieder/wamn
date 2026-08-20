//! Guards the build-environment pointer to the wash-runtime fork ledger.

use std::fs;
use std::path::{Path, PathBuf};

const ACTIVE_PLAN: &str = "docs/archive/PLAN/PLAN.md";
const BUILD_AND_TEST_DOC: &str = "docs/archive/build-and-test.md";
const CARGO_LOCK: &str = "Cargo.lock";
const FORK_LEDGER: &str = "docs/archive/platform/wash-runtime-fork.md";
const ROOT_MANIFEST: &str = "Cargo.toml";
const RUST_TOOLCHAIN: &str = "rust-toolchain.toml";
const UPGRADE_DELTA: &str = "docs/archive/PLAN/WASMCLOUD-UPGRADE-2.6.1.md";

const EXPECTED_BUILD_ENVIRONMENT_PREAMBLE: &str = r#"wamn-host builds against wash-runtime consumed as a **git dependency from our
fork** (dkkloimwieder/wasmCloud, branch `wamn/2.7.0` = upstream v2.7.0).
`docs/archive/platform/wash-runtime-fork.md` is the authoritative carried-policy ledger and
rev-bump runbook; this preamble does not duplicate its commit or seam
inventory. The rev is pinned in one place:
`workspace.dependencies.wash-runtime.rev` in the root `Cargo.toml`."#;
const EXPECTED_MANIFEST_LEDGER_COMMENT: &str = "# Upstream v2.7.0 plus the policies recorded in\n\
# docs/archive/platform/wash-runtime-fork.md. The ledger is authoritative.";
const EXPECTED_PLAN_REVISION: &str = "daba6029";
const EXPECTED_REVISION: &str = "daba602901507338e99f277e07a8e923c61dc557";
const EXPECTED_UPSTREAM_BASE: &str = "9561cb59759fa15b0a64bdb0b318255309aeddcd";
/// The carried policies in ledger order. `wamn/2.7.0` was carried by MERGE
/// rather than re-port (wamn-0h0g.15.20), so the first seven keep the SHAs they
/// were re-ported under at v2.6.1 and the last three are the v2.7.0 additions.
const POLICY_COMMITS: [&str; 10] = [
    "f90d977f", "24b220f5", "6ca3d6f7", "0d98f850", "a9f9c57d", "95b04ded", "33b24183", "1653858b",
    "d836cd3b", "fc4d2b22",
];
const EXIT_CONDITIONS: [&str; 10] = [
    "upstream ships native epoch-deadline support — delete the commit (the wamn-host ticker/config side stays as-is)",
    "upstream plumbs `memory_limit_mb` into a Store limiter — delete the commit",
    "upstream provides equivalent host-enforced P2/P3 trace-context injection across HTTP, gRPC, and custom transports, including client-span parenting — delete the commit",
    "upstream gates socket linking on `host_interfaces`, or consults an egress policy for `TcpConnect` — delete the commit",
    "upstream gates socket linking on `host_interfaces`, or consults an egress policy for UDP connect/datagram operations — delete the commit",
    "upstream exposes equivalent limiter introspection accessors — delete the commit",
    "upstream provides an equivalent inbound request-count metric, and the wamn dashboards, SLOs, and mutation gate have migrated to it and passed — delete the commit",
    "upstream invalidates a stopped workload's pooled egress state unconditionally, for every workload shape rather than only HTTP exporters — delete the commit",
    "upstream applies its own socket policy to plugin stores with a deny-unless-declared posture, or gates plugin socket linking on `host_interfaces` — delete the commit",
    "upstream offers a host-level pooling override that a workload manifest cannot widen — delete the commit",
];
/// Commits on the tip that are hygiene, not policy — they must never become
/// ledger rows. `f9fcf287`/`09b1132f` are the v2.6.1 pair; the rest are v2.7.0's.
const NON_POLICY_COMMITS: [&str; 5] = ["f9fcf287", "09b1132f", "01c60200", "f2c098ad", "daba6029"];

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
        lock_package.contains("version = \"2.7.0\"")
            && lock_package.contains(&format!(
                "source = \"git+https://github.com/dkkloimwieder/wasmCloud?rev={EXPECTED_REVISION}#{EXPECTED_REVISION}\""
            )),
        "Cargo.lock must resolve wash-runtime 2.7.0 from the exact fork revision"
    );

    assert!(
        ledger.contains(&format!(
            "Current: `wamn/2.7.0` = upstream v2.7.0\n  (`{EXPECTED_UPSTREAM_BASE}`)"
        )) && ledger.contains(&format!("final fork tip `{EXPECTED_REVISION}`"))
            && ledger.contains("## Carried commits (the ledger)")
            && ledger.contains("## Sync runbook"),
        "referenced fork document must record the current base, tip, policy ledger, and runbook"
    );
}

#[test]
fn fork_ledger_records_exact_ten_policy_commits_and_exit_conditions() {
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
        "ledger must contain the exact ten carried policy commits in ledger order"
    );
    for (row, exit_condition) in rows.iter().zip(EXIT_CONDITIONS) {
        assert!(
            row.contains(exit_condition),
            "carried-policy exit condition drifted: {exit_condition}"
        );
    }
    let raw_udp = rows
        .iter()
        .find(|row| row.contains("`a9f9c57d`"))
        .expect("carried-policy ledger must retain the raw-UDP row");
    for required in [
        "both Component and Service guests may `UdpBind` on loopback or unspecified addresses",
        "`raw_socket_opt_out_shapes_an_empty_allowlist_under_enforce`",
        "an empty allowlist under `Enforce`",
        "`egress_peers` stays empty",
        "discard every unsolicited off-box datagram",
        "Private per-workload virtual-network receive remains possible",
        "bounded wakeup and syscall cost",
    ] {
        assert!(
            raw_udp.contains(required),
            "raw-UDP ledger row lost accepted-posture evidence {required:?}"
        );
    }
    for stale in [
        "`UdpBind` is service-loopback-only",
        "denied for non-service components",
    ] {
        assert!(
            !raw_udp.contains(stale),
            "raw-UDP ledger row restored the rejected bind-posture claim {stale:?}"
        );
    }
    for hygiene in NON_POLICY_COMMITS {
        assert!(
            !commits.contains(&hygiene),
            "hygiene commit {hygiene} must not become a carried-policy row"
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
        "## Current upstream delta (v2.6.1 → v2.7.0)",
        "## Carried commits",
    );

    for required in [
        "git ls-remote --tags",
        // The v2.7.0 tag object and its peeled commit.
        "ecaa036ccc563ed6fadf0e74e4fcedd70e7cf3e1",
        EXPECTED_UPSTREAM_BASE,
        // Scale, and the command that reproduces the full list.
        "78-commit base bump",
        "git log --oneline df8a8bcd..9561cb59",
        // The four upstream themes that reach a carried policy: pooled egress
        // (why `DefaultOutgoingHandler` is now fielded), the single socket
        // decision point, warm pooled instances, and the no-trap plugin change.
        "0dcd9156",
        "82e06949",
        "03d621c0",
        "e9a3a80f",
        // Dependencies MOVED this time — the delta must say so explicitly.
        "47.0.1 → 47.0.3",
        "`async-nats` 0.49.1",
        "`rust-version` 1.94.0",
        "Exit conditions: all ten remain unsatisfied",
        // Anti-silent-drop evidence: a merge can keep a carried function while
        // upstream's rewrite bypasses its call site, so the delta records that
        // the trace injectors were verified still CALLED, not merely present.
        "inject_outbound_trace_context_p2",
    ] {
        assert!(
            delta.contains(required),
            "current upstream delta lost required evidence {required:?}"
        );
    }

    let current_sync = ledger
        .lines()
        .find(|line| line.starts_with("| 2026-08-17 |"))
        .expect("sync log must contain the v2.7.0 retarget record");
    for required in [
        "`4676add3`",
        "`1653858b`",
        "`d836cd3b`",
        "`fc4d2b22`",
        "merge",
        "47.0.3",
        "wamn-0h0g.15.20",
        // The fork-sync gate subset does not run on this branch; the record must
        // name where it does, rather than leaving the Gates column looking green.
        "wamn-0h0g.15.25",
    ] {
        assert!(
            current_sync.contains(required),
            "v2.7.0 sync record lost required evidence {required:?}"
        );
    }

    // The v2.6.1 upgrade record is retained history, not the current delta; its
    // tag verification must stay intact so the trail back through 2.6.1 holds.
    assert!(
        upgrade_delta.contains("'refs/tags/v2.6.1' 'refs/tags/v2.6.1^{}'")
            && !upgrade_delta.contains("refs/tags/runtime-operator/v2.6.1"),
        "retained v2.6.1 upgrade record must still verify the real tag and peeled ref"
    );
    assert!(
        manifest.contains("wasmtime-wasi = \"47.0.3\"")
            && manifest.contains("wasmtime-wasi-http = \"47.0.3\"")
            && manifest.contains("async-nats = { version = \"0.49.1\"")
            && rust_toolchain.contains("upstream wasmCloud's rust-version 1.94.0"),
        "workspace dependency and upstream MSRV documentation must match the recorded delta"
    );
}

#[test]
fn active_plan_points_to_the_current_fork_delta() {
    let root = repository_root();
    let plan = read_repository_file(&root, ACTIVE_PLAN);

    assert!(
        plan.contains("**The v2.7.0 upgrade is complete.**")
            && plan.contains("pinned at `wamn/2.7.0`, rev")
            && plan.contains(&format!("`{EXPECTED_PLAN_REVISION}`"))
            && plan.contains("`docs/archive/PLAN/WASMCLOUD-UPGRADE-2.6.1.md`")
            && plan.contains("`docs/archive/PLAN/WASMCLOUD-UPGRADE-2.6.0.md`"),
        "active roadmap must record the current fork pin and the retained upgrade records"
    );
    // The two syncs differ in KIND, and the roadmap has to keep them apart: the
    // v2.6.1 retarget absorbed renames at unchanged dependencies, while v2.7.0
    // moved the Wasmtime family. A reader who conflates them will size the next
    // sync from the wrong precedent.
    assert!(
        plan.contains("**policy re-port**, not a dependency bump")
            && plan.contains("78-commit base bump")
            && plan.contains("moved dependencies"),
        "active roadmap must distinguish the v2.6.1 policy re-port from the v2.7.0 base bump"
    );
}

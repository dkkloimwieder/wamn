//! 2.6 egress-review gate: static assertion that the shipped workload
//! components expose no raw-socket surface, so the `wamn:postgres` plugin (and
//! the `allowed_hosts`-gated, egress-spied `wasi:http` chokepoint from S6) are
//! the only egress paths a component can reach.
//!
//! WHY STATIC. This gate compiles each component and walks its import list; it
//! never opens a socket or touches Postgres. The result is therefore a pure
//! function of the wasm bytes and is identical in-cluster and locally — unlike
//! the timing/DB gates there is no separate in-cluster Job of record.
//!
//! WHAT IT PROVES. The "plugin is the only DB path" guarantee (docs 2.6) rests
//! on WIT-world composition: the runtime registers `wasi:sockets` on every
//! workload linker unconditionally (vendor wash-runtime `engine/mod.rs`) and the
//! production socket policy allows outbound TCP to any address
//! (`linked_call.rs` `build_ctx_from_template`: `socket_addr_check` returns
//! `true` for `TcpConnect`; `allowed_network_uses` defaults to `tcp: true`),
//! consulting `allowed_hosts` only for `wasi:http`. So the boundary is whether a
//! shipped component's world *imports* `wasi:sockets` at all. This gate asserts
//! it does not — and that the DB-touching runner imports `wamn:postgres`.
//! Enforcing that boundary at deploy (rejecting a socket-importing workload) is
//! the follow-up wamn-7j0.1; see docs/security-db-path.md.
//!
//! TWO PROFILES (E17). The verdict comes from `wamn_host::egress_guard` — the
//! same classifier the host publish-gate uses — not a forked local rule:
//! - **first-party flow-runner** legitimately imports `wamn:postgres` (the DB
//!   path); it is screened by the socket denylist (`egress_guard::denied_imports`,
//!   E13a) and must import the plugin.
//! - **tenant / custom-node** artifacts are held to the POSITIVE allowlist v1
//!   (`egress_guard::disallowed_tenant_imports`, docs/wamn-node-design-notes.md
//!   §9): any non-allowlisted package is refused — `wamn:postgres` most of all,
//!   since importing the plugin hands a tenant node the raw DB surface + the
//!   `DO`/`EXECUTE` claim-mutation bypass (docs/findings.md §3 E17). A denylist
//!   cannot express this: `wamn:postgres` is the *intended* path for the runner.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use clap::Args;
use wash_runtime::wasmtime::Engine as RawEngine;
use wash_runtime::wasmtime::component::Component as WasmtimeComponent;

use wamn_host::egress_guard::{denied_imports, disallowed_tenant_imports};
use wamn_host::engine::build_engine;

#[derive(Args)]
pub struct EgressBenchArgs {
    /// The standard flow-runner — the first-party DB-touching shipped workload.
    /// Must import `wamn:postgres` (the DB path) and must NOT import `wasi:sockets`.
    #[arg(long)]
    flowrunner: PathBuf,

    /// Tenant / custom-node artifacts. Held to the positive allowlist v1
    /// (`wamn:postgres` and `wasi:sockets` are refused). Repeatable.
    #[arg(long = "component")]
    components: Vec<PathBuf>,
}

/// Other host-plugin egress `namespace:package`s not expected in the first-party
/// flow-runner; if one appears it is flagged as a new egress path to justify.
/// (Tenant artifacts are held to the positive allowlist instead, which rejects
/// these by construction.)
const OTHER_EGRESS_PKGS: &[&str] = &[
    "wasi:blobstore",
    "wasi:keyvalue",
    "wasi:messaging",
    "wamn:messaging",
];

/// The `namespace:package` of an instance import. Imports look like
/// `wasi:sockets/tcp@0.2.3`; we key policy on the `ns:pkg` prefix.
fn ns_pkg(import_name: &str) -> &str {
    import_name.split('/').next().unwrap_or(import_name)
}

/// Compile `path` and return its full import NAMES (e.g. `wasi:sockets/tcp@0.2.3`)
/// — names, not just `ns:pkg`, so the shared `egress_guard` classifiers can name
/// the exact offending interface.
fn import_names(engine: &RawEngine, path: &Path) -> anyhow::Result<Vec<String>> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let component = WasmtimeComponent::new(engine, &bytes)
        .map_err(|e| anyhow::anyhow!("compile {}: {e}", path.display()))?;
    let ty = component.component_type();
    let eng = component.engine();
    Ok(ty
        .imports(eng)
        .map(|(name, _item)| name.to_string())
        .collect())
}

/// The distinct `namespace:package`s of `names`, for the human-readable summary.
fn ns_pkgs(names: &[String]) -> BTreeSet<&str> {
    names.iter().map(|n| ns_pkg(n)).collect()
}

/// First-party flow-runner profile: it legitimately imports `wamn:postgres` (the
/// DB path) and the `allowed_hosts`-gated `wasi:http` chokepoint, and must NOT
/// import raw sockets. The raw-socket verdict is the shared E13a guard
/// (`egress_guard::denied_imports`) — not a forked local rule. Returns whether
/// it passed.
fn assert_flowrunner(label: &str, names: &[String]) -> bool {
    let pkgs = ns_pkgs(names);
    let denied = denied_imports(names.iter().map(String::as_str));
    let other: Vec<&str> = pkgs
        .iter()
        .copied()
        .filter(|p| OTHER_EGRESS_PKGS.contains(p))
        .collect();

    println!("  {label}");
    println!("    packages: {pkgs:?}");

    let mut ok = true;
    if !denied.is_empty() {
        println!(
            "    FAIL: imports raw-socket interface(s) {denied:?} — can reach Postgres directly, \
             bypassing the plugin"
        );
        ok = false;
    }
    if !other.is_empty() {
        println!(
            "    FAIL: imports unexpected egress interface(s) {other:?} — new egress path, must be \
             justified / allowlisted"
        );
        ok = false;
    }
    if !names.iter().any(|n| ns_pkg(n) == "wamn:postgres") {
        println!(
            "    FAIL: does not import wamn:postgres — expected the DB-touching workload to use the \
             plugin"
        );
        ok = false;
    }
    if ok {
        println!("    PASS: no raw-socket surface; wamn:postgres is the DB path");
    }
    ok
}

/// Tenant / custom-node profile: held to the positive allowlist v1
/// (`egress_guard::disallowed_tenant_imports` — the shared classifier the host
/// publish-gate uses). Any non-allowlisted package is refused — most of all
/// `wamn:postgres` (raw DB surface + `DO`/`EXECUTE` claim-mutation bypass) and
/// `wasi:sockets`. Returns whether it passed.
fn assert_tenant(label: &str, names: &[String]) -> bool {
    let pkgs = ns_pkgs(names);
    let disallowed = disallowed_tenant_imports(names.iter().map(String::as_str));

    println!("  {label}");
    println!("    packages: {pkgs:?}");

    if disallowed.is_empty() {
        println!(
            "    PASS: imports only allowlisted packages (allowlist v1; no wamn:postgres, no sockets)"
        );
        true
    } else {
        println!(
            "    FAIL: imports non-allowlisted package(s) {disallowed:?} — tenant artifacts may \
             import only the allowlist v1 (excludes wamn:postgres + wasi:sockets)"
        );
        false
    }
}

pub async fn run(args: EgressBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    println!("# wamn-host 2.6/E17 egressbench — DB-path egress review (static)");
    println!("# claim: the wamn:postgres plugin is the only DB path. The first-party");
    println!("#        flow-runner imports it and no raw sockets; tenant / custom-node");
    println!("#        artifacts are held to the positive allowlist v1 (no wamn:postgres).");

    let engine = build_engine(&[])?;
    let raw: &RawEngine = engine.inner();

    let mut pass = true;

    println!("\n## first-party flow-runner (DB path)");
    let fr = import_names(raw, &args.flowrunner)?;
    pass &= assert_flowrunner(&format!("flow-runner  {}", args.flowrunner.display()), &fr);

    if !args.components.is_empty() {
        println!("\n## tenant / custom-node artifacts (allowlist v1)");
    }
    for path in &args.components {
        let names = import_names(raw, path)?;
        pass &= assert_tenant(&format!("component    {}", path.display()), &names);
    }

    println!("\negressbench complete — overall PASS: {pass}");
    if !pass {
        bail!(
            "2.6/E17 egress gate failed: a shipped workload exposes a raw-socket / non-allowlisted egress path"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ns_pkg_keeps_namespace_package() {
        assert_eq!(ns_pkg("wasi:sockets/tcp@0.2.3"), "wasi:sockets");
        assert_eq!(ns_pkg("wamn:postgres/client@0.1.0"), "wamn:postgres");
        assert_eq!(ns_pkg("wasi:http/outgoing-handler@0.2.3"), "wasi:http");
        // Degenerate: no interface segment.
        assert_eq!(ns_pkg("wasi:clocks"), "wasi:clocks");
    }

    #[test]
    fn flowrunner_shape_passes() {
        // flow-runner: DB plugin + chokepointed http, no raw sockets.
        let n = names(&[
            "wamn:postgres/client@0.1.0",
            "wasi:http/outgoing-handler@0.2.3",
            "wasi:clocks/monotonic-clock@0.2.3",
            "wasi:io/streams@0.2.3",
        ]);
        assert!(assert_flowrunner("runner", &n));
    }

    #[test]
    fn flowrunner_importing_sockets_fails() {
        // The boundary 2.6/E13a defends: a wasi:sockets import is a DB-path bypass.
        let n = names(&["wamn:postgres/client@0.1.0", "wasi:sockets/tcp@0.2.3"]);
        assert!(!assert_flowrunner("socket-runner", &n));
    }

    #[test]
    fn flowrunner_without_postgres_fails() {
        // The DB-touching workload must actually use the plugin.
        let n = names(&["wasi:cli/run@0.2.3"]);
        assert!(!assert_flowrunner("no-db", &n));
    }

    #[test]
    fn tenant_node_shape_passes() {
        // A standard custom node: SDK imports + determinism plumbing, no DB.
        let n = names(&[
            "wamn:node/credentials@0.1.0",
            "wasi:clocks/monotonic-clock@0.2.3",
            "wasi:io/streams@0.2.3",
            "wasi:cli/run@0.2.3",
        ]);
        assert!(assert_tenant("node", &n));
    }

    /// E17 — the fix of record. A TENANT artifact importing `wamn:postgres` (the
    /// raw DB surface + the DO/EXECUTE claim-mutation bypass) is REJECTED by the
    /// custom-node profile. The bug was that this shape PASSED (require_postgres
    /// was false and the socket-only classifier let `wamn:postgres` through).
    /// Mutation (a): re-adding `wamn:postgres` to `TENANT_ALLOWED_PKGS` flips
    /// this to a pass. Mutation (b): removing the `disallowed_tenant_imports`
    /// call from `assert_tenant` (bypassing the chokepoint) flips it too.
    #[test]
    fn tenant_importing_postgres_is_rejected() {
        let n = names(&["wamn:postgres/client@0.1.0", "wasi:io/streams@0.2.3"]);
        assert!(!assert_tenant("evil-node", &n));
    }

    /// E13a no-regression at the tenant layer: a socket importer is still refused
    /// (wasi:sockets is not on the allowlist).
    #[test]
    fn tenant_importing_sockets_is_rejected() {
        let n = names(&["wasi:sockets/tcp@0.2.3", "wasi:io/streams@0.2.3"]);
        assert!(!assert_tenant("socket-node", &n));
    }
}

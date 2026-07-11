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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use clap::Args;
use wash_runtime::wasmtime::Engine as RawEngine;
use wash_runtime::wasmtime::component::Component as WasmtimeComponent;

use crate::engine::build_engine;

#[derive(Args)]
pub struct EgressBenchArgs {
    /// The standard flow-runner — the DB-touching shipped workload. Must import
    /// `wamn:postgres` (the DB path) and must NOT import `wasi:sockets`.
    #[arg(long)]
    flowrunner: PathBuf,

    /// Additional guest components to sweep (custom-node / generic workload
    /// shapes). Each must NOT import `wasi:sockets`. Repeatable.
    #[arg(long = "component")]
    components: Vec<PathBuf>,
}

/// `namespace:package` of the raw-socket interfaces. A workload importing any of
/// these can open a TCP/UDP connection to Postgres directly, bypassing the
/// plugin's tenant-claim / RLS injection. This is the boundary 2.6 defends.
const RAW_SOCKET_PKGS: &[&str] = &["wasi:sockets"];

/// Egress-capable `namespace:package`s that ARE allowed: the DB plugin, and the
/// `allowed_hosts`-gated + egress-spied `wasi:http` chokepoint (S6).
const ALLOWED_EGRESS_PKGS: &[&str] = &["wamn:postgres", "wasi:http"];

/// Other host-plugin egress interfaces. Not expected in wamn workloads; if one
/// appears it is flagged — not necessarily a raw bypass, but a new egress path
/// that must be justified / allowlisted.
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

/// Compile `path` and return the set of `namespace:package`s it imports.
fn import_pkgs(engine: &RawEngine, path: &Path) -> anyhow::Result<BTreeSet<String>> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let component = WasmtimeComponent::new(engine, &bytes)
        .map_err(|e| anyhow::anyhow!("compile {}: {e}", path.display()))?;
    let ty = component.component_type();
    let eng = component.engine();
    let mut pkgs = BTreeSet::new();
    for (name, _item) in ty.imports(eng) {
        pkgs.insert(ns_pkg(name).to_string());
    }
    Ok(pkgs)
}

/// Assert one component's egress surface. Returns whether it passed.
fn assert_component(label: &str, pkgs: &BTreeSet<String>, require_postgres: bool) -> bool {
    let raw: Vec<&str> = pkgs
        .iter()
        .map(String::as_str)
        .filter(|p| RAW_SOCKET_PKGS.contains(p))
        .collect();
    let allowed: Vec<&str> = pkgs
        .iter()
        .map(String::as_str)
        .filter(|p| ALLOWED_EGRESS_PKGS.contains(p))
        .collect();
    let other: Vec<&str> = pkgs
        .iter()
        .map(String::as_str)
        .filter(|p| OTHER_EGRESS_PKGS.contains(p))
        .collect();

    println!("  {label}");
    println!("    egress imports: allowed={allowed:?} raw-socket={raw:?} other={other:?}");

    let mut ok = true;
    if !raw.is_empty() {
        println!(
            "    FAIL: imports raw-socket interface(s) {raw:?} — can reach Postgres directly, \
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
    if require_postgres && !pkgs.iter().any(|p| p == "wamn:postgres") {
        println!(
            "    FAIL: does not import wamn:postgres — expected the DB-touching workload to use the \
             plugin"
        );
        ok = false;
    }
    if ok {
        let tail = if require_postgres {
            "; wamn:postgres is the DB path"
        } else {
            ""
        };
        println!("    PASS: no raw-socket surface{tail}");
    }
    ok
}

pub async fn run(args: EgressBenchArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    println!("# wamn-host 2.6 egressbench — DB-path egress review (static)");
    println!("# claim: the wamn:postgres plugin is the only DB path; no shipped");
    println!("#        workload imports wasi:sockets (raw TCP/UDP to Postgres).");

    let engine = build_engine(&[])?;
    let raw: &RawEngine = engine.inner();

    let mut pass = true;

    let fr = import_pkgs(raw, &args.flowrunner)?;
    pass &= assert_component(
        &format!("flow-runner  {}", args.flowrunner.display()),
        &fr,
        true,
    );

    for path in &args.components {
        let pkgs = import_pkgs(raw, path)?;
        pass &= assert_component(&format!("component    {}", path.display()), &pkgs, false);
    }

    println!("\negressbench complete — overall PASS: {pass}");
    if !pass {
        bail!(
            "2.6 egress gate failed: a shipped workload exposes a raw-socket / unexpected egress path"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkgs(items: &[&str]) -> BTreeSet<String> {
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
    fn runner_shape_passes() {
        // flow-runner: DB plugin + chokepointed http, no raw sockets.
        let p = pkgs(&["wamn:postgres", "wasi:http", "wasi:clocks", "wasi:io"]);
        assert!(assert_component("runner", &p, true));
    }

    #[test]
    fn node_shape_passes_without_postgres() {
        let p = pkgs(&["wamn:node", "wasi:cli"]);
        assert!(assert_component("node", &p, false));
    }

    #[test]
    fn raw_socket_import_fails() {
        // The boundary 2.6 defends: a wasi:sockets import is a DB-path bypass.
        let p = pkgs(&["wamn:postgres", "wasi:sockets"]);
        assert!(!assert_component("socket-importer", &p, true));
    }

    #[test]
    fn unexpected_egress_import_fails() {
        // A new host-plugin egress path that is not on the allowlist is flagged.
        let p = pkgs(&["wasi:keyvalue"]);
        assert!(!assert_component("kv", &p, false));
    }

    #[test]
    fn db_workload_without_postgres_fails() {
        // The DB-touching workload must actually use the plugin.
        let p = pkgs(&["wasi:cli"]);
        assert!(!assert_component("no-db", &p, true));
    }
}

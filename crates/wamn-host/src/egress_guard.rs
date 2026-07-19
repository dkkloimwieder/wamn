//! Publish-time egress guard: refuse any component whose world imports
//! `wasi:sockets` (E13a).
//!
//! The runtime links `wasi:sockets` (tcp/udp/create-socket/instance-network/
//! network/ip_name_lookup) on **every** workload linker unconditionally, and
//! its `TcpConnect` policy is allow-all — it never consults the per-flow
//! `allowed_hosts` egress allowlist, which governs `wasi:http` ONLY (see
//! docs/security-db-path.md, docs/findings.md §3 E13). So a component that
//! *imports* `wasi:sockets` gets arbitrary outbound TCP with DNS, bypassing the
//! `wamn:postgres` tenant-claim / RLS path. The effective boundary is therefore
//! whether a component's world imports the interface at all — and until the
//! runtime half (a binary opt-in `TcpConnect` policy, the fork commit) lands,
//! the build/publish side owns the enforcement.
//!
//! This module is that enforcement: a single structural rule — reject a
//! component that imports any interface of the `wasi:sockets` package — reusable
//! by any wamn build/publish path that has the component bytes, and driven by
//! the `socketguard` refusal gate (crates/wamn-gates). It intentionally keys on
//! the WIT `namespace:package` (`wasi:sockets`), not fragile interface-name
//! matching: every socket interface (`wasi:sockets/tcp@…`, `…/udp@…`,
//! `…/ip-name-lookup@…`, a bare `wasi:sockets@…`) collapses to the same package
//! and is caught by the one rule.

use wash_runtime::wasmtime::Engine as RawEngine;
use wash_runtime::wasmtime::component::Component as WasmtimeComponent;

/// The denied WIT `namespace:package`. A component importing ANY interface of
/// this package can open a raw TCP/UDP socket and reach Postgres directly,
/// bypassing the `wamn:postgres` plugin's tenant-claim / RLS injection. This is
/// the single load-bearing literal — pinned by a drift guard in the tests.
pub const DENIED_EGRESS_PKG: &str = "wasi:sockets";

/// Refusal of a component whose world imports the denied egress package.
#[derive(Debug)]
pub enum EgressGuardError {
    /// The component imports one or more `wasi:sockets` interfaces (named in
    /// `imports`, in the component's own declaration order).
    RawSocketImport {
        /// Caller-supplied label for the offending component (path / name).
        component: String,
        /// The full offending import names, e.g. `wasi:sockets/tcp@0.2.3`.
        imports: Vec<String>,
    },
}

impl std::fmt::Display for EgressGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EgressGuardError::RawSocketImport { component, imports } => write!(
                f,
                "component {component:?} imports raw-socket interface(s) {imports:?} \
                 (package {DENIED_EGRESS_PKG:?}) — this opens arbitrary outbound TCP with DNS, \
                 bypassing the wamn:postgres tenant-claim / RLS path; the platform refuses to \
                 publish it"
            ),
        }
    }
}

impl std::error::Error for EgressGuardError {}

/// The WIT `namespace:package` an import name belongs to. Import names look
/// like `wasi:sockets/tcp@0.2.3` (an interface) or `wasi:sockets@0.2.3` (a bare
/// package); both key on `wasi:sockets`. Strip the interface segment (`/…`)
/// first, then any package version (`@…`).
fn import_pkg(import_name: &str) -> &str {
    let head = import_name.split('/').next().unwrap_or(import_name);
    head.split('@').next().unwrap_or(head)
}

/// The subset of `import_names` that import the denied egress package, in the
/// order given. This is the one structural rule; [`screen_component`] and the
/// gate both go through it. Empty result == the component clears the guard.
pub fn denied_imports<'a>(import_names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    import_names
        .into_iter()
        .filter(|name| import_pkg(name) == DENIED_EGRESS_PKG)
        .map(str::to_string)
        .collect()
}

/// Screen a compiled component: `Err` iff its world imports the denied egress
/// package. `label` names the component in the refusal.
pub fn screen_compiled(component: &WasmtimeComponent, label: &str) -> Result<(), EgressGuardError> {
    let engine = component.engine();
    let ty = component.component_type();
    let imports: Vec<String> = ty
        .imports(engine)
        .map(|(name, _)| name.to_string())
        .collect();
    let denied = denied_imports(imports.iter().map(String::as_str));
    if denied.is_empty() {
        Ok(())
    } else {
        Err(EgressGuardError::RawSocketImport {
            component: label.to_string(),
            imports: denied,
        })
    }
}

/// Compile `wasm` on `engine` and screen it (the publish-path entry point):
/// `Err` if the bytes do not compile, or if the component's world imports the
/// denied egress package.
pub fn screen_component(engine: &RawEngine, wasm: &[u8], label: &str) -> anyhow::Result<()> {
    let component = WasmtimeComponent::new(engine, wasm)
        .map_err(|e| anyhow::anyhow!("compile {label}: {e}"))?;
    screen_compiled(&component, label)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drift guard: the one load-bearing literal is the denied package. If this
    /// string moves, the guard silently stops matching the interface the
    /// runtime links unconditionally.
    #[test]
    fn denied_package_literal_is_pinned() {
        assert_eq!(DENIED_EGRESS_PKG, "wasi:sockets");
    }

    #[test]
    fn import_pkg_strips_interface_and_version() {
        assert_eq!(import_pkg("wasi:sockets/tcp@0.2.3"), "wasi:sockets");
        assert_eq!(
            import_pkg("wasi:sockets/ip-name-lookup@0.2.3"),
            "wasi:sockets"
        );
        assert_eq!(import_pkg("wasi:sockets@0.2.3"), "wasi:sockets");
        assert_eq!(import_pkg("wamn:postgres/client@0.1.0"), "wamn:postgres");
        assert_eq!(import_pkg("wasi:clocks"), "wasi:clocks");
    }

    /// Every socket interface the runtime links unconditionally collapses to
    /// the one denied package — the one rule catches all of them. This is the
    /// mutant target: neutering [`denied_imports`] returns an empty vec here.
    #[test]
    fn denied_imports_flags_every_socket_interface() {
        let names = [
            "wasi:sockets/tcp@0.2.3",
            "wasi:sockets/udp@0.2.3",
            "wasi:sockets/tcp-create-socket@0.2.3",
            "wasi:sockets/udp-create-socket@0.2.3",
            "wasi:sockets/instance-network@0.2.3",
            "wasi:sockets/network@0.2.3",
            "wasi:sockets/ip-name-lookup@0.2.3",
        ];
        assert_eq!(denied_imports(names).len(), names.len());
    }

    /// A standard workload — the DB plugin, the `allowed_hosts`-gated http
    /// chokepoint, clocks/io — imports nothing denied.
    #[test]
    fn denied_imports_passes_standard_workload() {
        let names = [
            "wamn:postgres/client@0.1.0",
            "wasi:http/outgoing-handler@0.2.3",
            "wasi:clocks/monotonic-clock@0.2.3",
            "wasi:io/streams@0.2.3",
            "wamn:node/credentials@0.1.0",
        ];
        assert!(denied_imports(names).is_empty());
    }

    /// The offending imports are reported in declaration order, named in full.
    #[test]
    fn denied_imports_preserves_order_and_full_names() {
        let names = [
            "wasi:clocks/wall-clock@0.2.3",
            "wasi:sockets/tcp@0.2.3",
            "wamn:postgres/client@0.1.0",
            "wasi:sockets/ip-name-lookup@0.2.3",
        ];
        assert_eq!(
            denied_imports(names),
            vec![
                "wasi:sockets/tcp@0.2.3".to_string(),
                "wasi:sockets/ip-name-lookup@0.2.3".to_string(),
            ]
        );
    }
}

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
//!
//! ## Tenant / custom-node allowlist (E17)
//!
//! A TENANT (custom-node) artifact is held to a *positive* allowlist instead of
//! the socket denylist above: it may import only [`TENANT_ALLOWED_PKGS`]
//! (allowlist v1, docs/wamn-node-design-notes.md §9), and anything else is
//! refused — most load-bearingly `wamn:postgres` (the raw DB surface + the
//! `DO`/`EXECUTE` claim-mutation bypass, docs/findings.md §3 E17) and
//! `wasi:sockets`, neither of which is on the list. A denylist alone could not
//! stop `wamn:postgres`, since that IS the intended DB path for first-party
//! workloads. So the two populations are screened differently:
//! [`screen_component`] (socket denylist) for the first-party flow-runner, which
//! legitimately imports `wamn:postgres`; [`screen_tenant_component`] (positive
//! allowlist) for tenant artifacts. Both classifiers share [`import_pkg`], and
//! both the `egressbench` publish-gate backstop (crates/wamn-gates) and any host
//! publish path go through these same functions — one classifier, not a fork.

use wash_runtime::wasmtime::Engine as RawEngine;
use wash_runtime::wasmtime::component::Component as WasmtimeComponent;

/// The denied WIT `namespace:package`. A component importing ANY interface of
/// this package can open a raw TCP/UDP socket and reach Postgres directly,
/// bypassing the `wamn:postgres` plugin's tenant-claim / RLS injection. This is
/// the single load-bearing literal — pinned by a drift guard in the tests.
pub const DENIED_EGRESS_PKG: &str = "wasi:sockets";

/// Refusal of a component whose world imports a denied egress surface.
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
    /// A TENANT / custom-node component imports package(s) outside the allowlist
    /// v1 ([`TENANT_ALLOWED_PKGS`]). Load-bearing exclusions: `wamn:postgres`
    /// (raw DB surface + `DO`/`EXECUTE` claim-mutation bypass) and `wasi:sockets`.
    DisallowedTenantImport {
        /// Caller-supplied label for the offending component (path / name).
        component: String,
        /// The full offending import names, in the component's declaration order.
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
            EgressGuardError::DisallowedTenantImport { component, imports } => write!(
                f,
                "tenant component {component:?} imports non-allowlisted package(s) via {imports:?} \
                 — a custom-node artifact may import only the allowlist v1 {TENANT_ALLOWED_PKGS:?}; \
                 wamn:postgres (raw DB surface + DO/EXECUTE claim-mutation bypass) and wasi:sockets \
                 are excluded; the platform refuses to publish it"
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

/// Tenant / custom-node import allowlist v1 — the WIT `namespace:package`s a
/// TENANT artifact may import (docs/wamn-node-design-notes.md §9, the 5.4
/// freeze). Keyed on `namespace:package`, like [`DENIED_EGRESS_PKG`]. A tenant
/// component importing anything OUTSIDE this set is refused at publish — most
/// load-bearingly `wamn:postgres` (raw DB surface + `DO`/`EXECUTE` claim-mutation
/// bypass) and `wasi:sockets`, neither of which is on the list. The interface-
/// level tightening (`wamn:node` → only `payloads`/`credentials`/`control`,
/// `wasi:http` → only `outgoing-handler`) is the mechanical builder lint (5.5);
/// this package-level backstop is what the publish gate can enforce today.
pub const TENANT_ALLOWED_PKGS: &[&str] = &[
    "wamn:node",       // payloads / credentials / control (notes 7-8; inert until 5.9-5.12)
    "wasi:clocks",     // virtualized time (determinism)
    "wasi:random",     // seeded randomness (determinism)
    "wasi:io",         // streams / poll
    "wasi:cli",        // std shim
    "wasi:filesystem", // std shim
    "wasi:http",       // outgoing-handler — builder prompts for allowedHosts
];

/// The subset of `import_names` a TENANT artifact may NOT import — every import
/// whose package is not on [`TENANT_ALLOWED_PKGS`] — in the order given. This is
/// the positive-allowlist classifier; [`screen_tenant_compiled`] and the
/// `egressbench` custom-node profile both go through it (one classifier, not a
/// fork). Empty result == the tenant component clears the guard.
pub fn disallowed_tenant_imports<'a>(
    import_names: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    import_names
        .into_iter()
        .filter(|name| !TENANT_ALLOWED_PKGS.contains(&import_pkg(name)))
        .map(str::to_string)
        .collect()
}

/// Screen a compiled TENANT / custom-node component against the allowlist v1:
/// `Err` iff its world imports any package outside [`TENANT_ALLOWED_PKGS`].
/// `label` names the component in the refusal.
pub fn screen_tenant_compiled(
    component: &WasmtimeComponent,
    label: &str,
) -> Result<(), EgressGuardError> {
    let engine = component.engine();
    let ty = component.component_type();
    let imports: Vec<String> = ty
        .imports(engine)
        .map(|(name, _)| name.to_string())
        .collect();
    let disallowed = disallowed_tenant_imports(imports.iter().map(String::as_str));
    if disallowed.is_empty() {
        Ok(())
    } else {
        Err(EgressGuardError::DisallowedTenantImport {
            component: label.to_string(),
            imports: disallowed,
        })
    }
}

/// Compile `wasm` on `engine` and screen it as a TENANT artifact (the tenant
/// publish-path entry point): `Err` if the bytes do not compile, or if the
/// component imports any package outside [`TENANT_ALLOWED_PKGS`].
pub fn screen_tenant_component(engine: &RawEngine, wasm: &[u8], label: &str) -> anyhow::Result<()> {
    let component = WasmtimeComponent::new(engine, wasm)
        .map_err(|e| anyhow::anyhow!("compile {label}: {e}"))?;
    screen_tenant_compiled(&component, label)?;
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

    /// Drift guard (E17): the two load-bearing exclusions must NOT be on the
    /// tenant allowlist — `wamn:postgres` (the raw DB surface + DO/EXECUTE
    /// claim-mutation bypass) and `wasi:sockets` (the E13a raw-TCP bypass).
    /// Mutation (a) target: adding `wamn:postgres` here flips the first assert.
    #[test]
    fn tenant_allowlist_excludes_postgres_and_sockets() {
        assert!(!TENANT_ALLOWED_PKGS.contains(&"wamn:postgres"));
        assert!(!TENANT_ALLOWED_PKGS.contains(&DENIED_EGRESS_PKG)); // wasi:sockets
    }

    /// E17: a tenant node importing the DB plugin — the raw DB surface + the
    /// DO/EXECUTE claim-mutation bypass — is refused, naming the exact import.
    /// Mutation (a) target: adding `wamn:postgres` to the allowlist empties this.
    #[test]
    fn disallowed_tenant_imports_flags_postgres() {
        let names = ["wamn:postgres/client@0.1.0", "wasi:io/streams@0.2.3"];
        assert_eq!(
            disallowed_tenant_imports(names),
            vec!["wamn:postgres/client@0.1.0".to_string()]
        );
    }

    /// E13a no-regression at the tenant layer: `wasi:sockets` is not on the
    /// allowlist, so a socket-importing tenant artifact is refused too.
    #[test]
    fn disallowed_tenant_imports_flags_sockets() {
        let names = [
            "wasi:sockets/tcp@0.2.3",
            "wasi:clocks/monotonic-clock@0.2.3",
        ];
        assert_eq!(
            disallowed_tenant_imports(names),
            vec!["wasi:sockets/tcp@0.2.3".to_string()]
        );
    }

    /// A standard custom node — the SDK imports (payloads/credentials/control),
    /// determinism plumbing (clocks/random), io, the std shims, and the
    /// `allowed_hosts`-gated http chokepoint — clears the allowlist.
    #[test]
    fn disallowed_tenant_imports_passes_standard_node() {
        let names = [
            "wamn:node/payloads@0.1.0",
            "wamn:node/credentials@0.1.0",
            "wamn:node/control@0.1.0",
            "wasi:clocks/monotonic-clock@0.2.3",
            "wasi:random/random@0.2.3",
            "wasi:io/streams@0.2.3",
            "wasi:cli/run@0.2.3",
            "wasi:filesystem/types@0.2.3",
            "wasi:http/outgoing-handler@0.2.3",
        ];
        assert!(disallowed_tenant_imports(names).is_empty());
    }
}

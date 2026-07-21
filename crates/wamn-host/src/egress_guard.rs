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
    /// A tenant node imports an interface OUTSIDE the 5.5 interface-level
    /// tightening within an otherwise-allowlisted package — a `wamn:node`
    /// interface beyond [`NODE_ALLOWED_INTERFACES`], or a `wasi:http` interface
    /// beyond [`HTTP_ALLOWED_INTERFACES`]. The builder lint ([`screen_builder_compiled`])
    /// refuses it; the package-level classifiers cannot express this.
    DisallowedNodeInterface {
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
            EgressGuardError::DisallowedNodeInterface { component, imports } => write!(
                f,
                "custom node {component:?} imports interface(s) {imports:?} outside the 5.5 \
                 interface-level allowlist — within wamn:node only {NODE_ALLOWED_INTERFACES:?} are \
                 importable and within wasi:http only {HTTP_ALLOWED_INTERFACES:?}; the builder \
                 refuses it"
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

// ---------------------------------------------------------------------------
// 5.5a — the builder's INTERFACE-level import lint + derived grants
// ---------------------------------------------------------------------------
//
// The package-level classifiers above are what the publish gate can enforce
// TODAY; the 5.5 builder tightens WITHIN the allowlisted packages, the lint the
// egress_guard's own doc (crates/wamn-host/src/egress_guard.rs, the E13a socket
// note) defers to 5.5. Two additions, both pure:
//   1. an interface-level lint — within `wamn:node` only payloads/credentials/
//      control, within `wasi:http` only outgoing-handler, `wasi:sockets`
//      forbidden outright (already off the package allowlist); and
//   2. `derive_grants` — import set -> the derived host-interface grants plus
//      the allowedHosts REQUIREMENT (present iff wasi:http/outgoing-handler is
//      imported, refused otherwise). Grants are DERIVED, never declared twice
//      (docs/wamn-node-design-notes.md note 7).

/// Within the `wamn:node` package a custom node may import ONLY these
/// interfaces (5.5 interface-level tightening; the frozen `docs/wamn-node.wit`
/// stream-node/http-node worlds import exactly the capability subset).
/// `handler` is an EXPORT, never imported. `types` is NOT a capability: it is
/// the type-only instance import wit-bindgen materializes because the exported
/// `handler` `use`s its types — every real node artifact carries it (the
/// componentized sample-node does), it grants nothing, and it is deliberately
/// absent from [`GRANTABLE_HOST_INTERFACES`].
pub const NODE_ALLOWED_INTERFACES: &[&str] = &["types", "payloads", "credentials", "control"];

/// Within the `wasi:http` package a custom node may import ONLY the outbound
/// chokepoint (`outgoing-handler`) — never `incoming-handler` / `types` / a
/// bare `wasi:http`. The `allowed_hosts` egress allowlist governs it.
pub const HTTP_ALLOWED_INTERFACES: &[&str] = &["outgoing-handler"];

/// The host-interface imports that BECOME derived grants (design-note 7): the
/// capability interfaces a node is granted at runtime, keyed `pkg/interface`,
/// version-stripped. Determinism / std shims (clocks / random / io / cli /
/// filesystem) are ambient plumbing, not grants.
pub const GRANTABLE_HOST_INTERFACES: &[&str] = &[
    "wamn:node/payloads",
    "wamn:node/credentials",
    "wamn:node/control",
    "wasi:http/outgoing-handler",
];

/// The one grant that is CONDITIONAL on a runtime argument: importing
/// `wasi:http/outgoing-handler` REQUIRES a non-empty `allowedHosts` (and its
/// absence REFUSES one). The single load-bearing literal of [`derive_grants`].
pub const HTTP_OUTGOING_HANDLER: &str = "wasi:http/outgoing-handler";

/// The interface segment of an import name (`wamn:node/credentials@0.1.0` ->
/// `credentials`); `None` for a bare package import (`wasi:clocks`).
fn import_interface(import_name: &str) -> Option<&str> {
    let after = import_name.split_once('/')?.1;
    Some(after.split('@').next().unwrap_or(after))
}

/// The `pkg/interface` key of an import name, version-stripped
/// (`wasi:http/outgoing-handler@0.2.6` -> `wasi:http/outgoing-handler`).
fn import_pkg_interface(import_name: &str) -> &str {
    import_name.split('@').next().unwrap_or(import_name)
}

/// The subset of `import_names` that violate the 5.5 INTERFACE-level tightening
/// WITHIN an otherwise-allowlisted package: a `wamn:node` interface outside
/// [`NODE_ALLOWED_INTERFACES`], or a `wasi:http` interface outside
/// [`HTTP_ALLOWED_INTERFACES`]. Package-level violations (`wasi:sockets`,
/// `wamn:postgres`, …) are NOT this function's concern —
/// [`disallowed_tenant_imports`] catches them first. Empty result == the
/// interface tightening passes.
pub fn disallowed_node_interfaces<'a>(
    import_names: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    import_names
        .into_iter()
        .filter(|name| match import_pkg(name) {
            "wamn:node" => {
                import_interface(name).is_none_or(|i| !NODE_ALLOWED_INTERFACES.contains(&i))
            }
            "wasi:http" => {
                import_interface(name).is_none_or(|i| !HTTP_ALLOWED_INTERFACES.contains(&i))
            }
            _ => false,
        })
        .map(str::to_string)
        .collect()
}

/// Screen a compiled component through the BUILDER lint (5.5): BOTH the tenant
/// package allowlist ([`disallowed_tenant_imports`]) AND the interface-level
/// tightening ([`disallowed_node_interfaces`]). Two arms in order — a package
/// violation (`wasi:sockets` / `wamn:postgres` / …) is reported first, then an
/// interface violation. `label` names the component. This is the strictest
/// screen: a node the builder will push must clear it.
pub fn screen_builder_compiled(
    component: &WasmtimeComponent,
    label: &str,
) -> Result<(), EgressGuardError> {
    let engine = component.engine();
    let ty = component.component_type();
    let imports: Vec<String> = ty
        .imports(engine)
        .map(|(name, _)| name.to_string())
        .collect();
    // Arm 1 — the package allowlist (wasi:sockets / wamn:postgres / …). Removing
    // this arm lets a wasi:sockets importer slip through (the mutation-(a) target).
    let pkg_bad = disallowed_tenant_imports(imports.iter().map(String::as_str));
    if !pkg_bad.is_empty() {
        return Err(EgressGuardError::DisallowedTenantImport {
            component: label.to_string(),
            imports: pkg_bad,
        });
    }
    // Arm 2 — the interface tightening within wamn:node / wasi:http.
    let iface_bad = disallowed_node_interfaces(imports.iter().map(String::as_str));
    if !iface_bad.is_empty() {
        return Err(EgressGuardError::DisallowedNodeInterface {
            component: label.to_string(),
            imports: iface_bad,
        });
    }
    Ok(())
}

/// Compile `wasm` on `engine` and screen it through the builder lint (the
/// builder's entry point): `Err` if the bytes do not compile, or if the
/// component fails the package allowlist or the interface tightening.
pub fn screen_builder_component(
    engine: &RawEngine,
    wasm: &[u8],
    label: &str,
) -> anyhow::Result<()> {
    let component = WasmtimeComponent::new(engine, wasm)
        .map_err(|e| anyhow::anyhow!("compile {label}: {e}"))?;
    screen_builder_compiled(&component, label)?;
    Ok(())
}

/// The grants DERIVED from a component's imports (design-note 7): the set of
/// host-interface grants, plus whether an `allowedHosts` egress allowlist is
/// REQUIRED. Grants are derived from the WIT imports, never declared twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedGrants {
    /// The grantable host interfaces the node imports, in the component's
    /// declaration order, keyed `pkg/interface` (version-stripped) — e.g.
    /// `wasi:http/outgoing-handler`, `wamn:node/credentials`.
    pub host_interfaces: Vec<String>,
    /// True iff the node imports `wasi:http/outgoing-handler`: an `allowedHosts`
    /// egress allowlist is then REQUIRED (and must be REFUSED / empty otherwise).
    pub requires_allowed_hosts: bool,
}

/// Derive the runtime grants from a component's import names (design-note 7):
/// the [`GRANTABLE_HOST_INTERFACES`] the node imports become its host-interface
/// grants, and importing `wasi:http/outgoing-handler` sets
/// [`DerivedGrants::requires_allowed_hosts`]. Pure over the import-name list —
/// the same list [`screen_builder_compiled`] walks.
pub fn derive_grants<'a>(import_names: impl IntoIterator<Item = &'a str>) -> DerivedGrants {
    let mut host_interfaces = Vec::new();
    let mut requires_allowed_hosts = false;
    for name in import_names {
        let key = import_pkg_interface(name);
        if GRANTABLE_HOST_INTERFACES.contains(&key) {
            if key == HTTP_OUTGOING_HANDLER {
                requires_allowed_hosts = true;
            }
            host_interfaces.push(key.to_string());
        }
    }
    DerivedGrants {
        host_interfaces,
        requires_allowed_hosts,
    }
}

/// Compile `wasm` and [`derive_grants`] from its imports (the builder's
/// deployment-emission path, 5.5f). `Err` only if the bytes do not compile —
/// grant derivation itself is total.
pub fn derive_grants_from_component(
    engine: &RawEngine,
    wasm: &[u8],
    label: &str,
) -> anyhow::Result<DerivedGrants> {
    let component = WasmtimeComponent::new(engine, wasm)
        .map_err(|e| anyhow::anyhow!("compile {label}: {e}"))?;
    let eng = component.engine();
    let ty = component.component_type();
    let imports: Vec<String> = ty.imports(eng).map(|(name, _)| name.to_string()).collect();
    Ok(derive_grants(imports.iter().map(String::as_str)))
}

/// A mismatch between a component's DERIVED grants and a declared `allowedHosts`
/// list — the "derived, never declared twice" rule (design-note 7): an
/// `allowedHosts` grant is REQUIRED iff `wasi:http` is imported, and REFUSED
/// otherwise.
#[derive(Debug)]
pub enum GrantError {
    /// The node imports `wasi:http/outgoing-handler` but no `allowedHosts` were
    /// declared — an http node with a deny-all egress list is a
    /// misconfiguration the builder refuses.
    AllowedHostsRequired {
        /// Caller-supplied label for the component (path / name).
        component: String,
    },
    /// `allowedHosts` were declared but the node imports no `wasi:http` — the
    /// grant is spurious (nothing to gate), so the builder refuses it rather
    /// than declare a grant for a capability the component lacks.
    AllowedHostsRefused {
        /// Caller-supplied label for the component (path / name).
        component: String,
        /// The spuriously-declared hosts.
        declared: Vec<String>,
    },
}

impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrantError::AllowedHostsRequired { component } => write!(
                f,
                "custom node {component:?} imports {HTTP_OUTGOING_HANDLER:?} but no allowedHosts \
                 were declared — an http node needs a non-empty egress allowlist (pass --allowed-host)"
            ),
            GrantError::AllowedHostsRefused {
                component,
                declared,
            } => write!(
                f,
                "custom node {component:?} declared allowedHosts {declared:?} but imports no \
                 wasi:http — the grant is spurious; grants are DERIVED from imports, never declared \
                 for a capability the component lacks"
            ),
        }
    }
}

impl std::error::Error for GrantError {}

/// Check a declared `allowedHosts` list against a component's derived grants
/// (design-note 7): REQUIRED iff `wasi:http` is imported, REFUSED otherwise.
/// `label` names the component in the refusal.
pub fn check_allowed_hosts_grant(
    grants: &DerivedGrants,
    allowed_hosts: &[String],
    label: &str,
) -> Result<(), GrantError> {
    match (grants.requires_allowed_hosts, allowed_hosts.is_empty()) {
        (true, true) => Err(GrantError::AllowedHostsRequired {
            component: label.to_string(),
        }),
        (false, false) => Err(GrantError::AllowedHostsRefused {
            component: label.to_string(),
            declared: allowed_hosts.to_vec(),
        }),
        _ => Ok(()),
    }
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

    // -----------------------------------------------------------------------
    // 5.5a — interface-level lint + derived grants
    // -----------------------------------------------------------------------

    /// Synthesize a minimal, valid component whose world imports exactly
    /// `import_names` (each as an empty instance) — the socketguard pattern,
    /// enough for the guard to walk the import NAMES.
    fn synth_component(import_names: &[&str]) -> Vec<u8> {
        use wasm_encoder::{
            Component, ComponentImportSection, ComponentTypeRef, ComponentTypeSection, InstanceType,
        };
        let mut types = ComponentTypeSection::new();
        for _ in import_names {
            types.instance(&InstanceType::new());
        }
        let mut imports = ComponentImportSection::new();
        for (i, name) in import_names.iter().enumerate() {
            imports.import(*name, ComponentTypeRef::Instance(i as u32));
        }
        let mut component = Component::new();
        component.section(&types);
        component.section(&imports);
        component.finish()
    }

    /// Screen synthesized bytes through the builder lint on the production engine.
    fn screen_builder(import_names: &[&str], label: &str) -> Result<(), EgressGuardError> {
        let engine = crate::engine::build_engine(&[]).expect("engine");
        let bytes = synth_component(import_names);
        let component =
            WasmtimeComponent::new(engine.inner(), &bytes).expect("compile synthesized");
        screen_builder_compiled(&component, label)
    }

    #[test]
    fn import_interface_extracts_the_middle_segment() {
        assert_eq!(
            import_interface("wamn:node/credentials@0.1.0"),
            Some("credentials")
        );
        assert_eq!(
            import_interface("wasi:http/outgoing-handler@0.2.6"),
            Some("outgoing-handler")
        );
        assert_eq!(import_interface("wasi:clocks"), None);
    }

    /// The interface tightening flags a `wasi:http` interface OTHER than
    /// `outgoing-handler` (here `types` + `incoming-handler`) and a `wamn:node`
    /// interface outside {payloads,credentials,control} (`handler` is an export,
    /// but a stray `wamn:node/types` import is caught), leaving the allowlisted
    /// interfaces alone.
    #[test]
    fn disallowed_node_interfaces_flags_off_allowlist_interfaces() {
        let names = [
            "wasi:http/outgoing-handler@0.2.6",  // allowed
            "wasi:http/types@0.2.6",             // refused
            "wasi:http/incoming-handler@0.2.6",  // refused
            "wamn:node/credentials@0.1.0",       // allowed
            "wamn:node/types@0.1.0",             // allowed (type-only, grants nothing)
            "wamn:node/handler@0.1.0",           // refused (an EXPORT, never imported)
            "wasi:clocks/monotonic-clock@0.2.3", // untouched (other package)
        ];
        assert_eq!(
            disallowed_node_interfaces(names),
            vec![
                "wasi:http/types@0.2.6".to_string(),
                "wasi:http/incoming-handler@0.2.6".to_string(),
                "wamn:node/handler@0.1.0".to_string(),
            ]
        );
    }

    /// A standard http-node world (`docs/wamn-node.wit`) clears the interface
    /// tightening: wasi:http/outgoing-handler + credentials + control.
    #[test]
    fn disallowed_node_interfaces_passes_http_node_world() {
        let names = [
            "wasi:http/outgoing-handler@0.2.6",
            "wamn:node/credentials@0.1.0",
            "wamn:node/control@0.1.0",
        ];
        assert!(disallowed_node_interfaces(names).is_empty());
    }

    /// The builder lint over a REAL synthesized component: a standard http-node
    /// world (the frozen `world http-node`) clears BOTH arms.
    #[test]
    fn builder_lint_passes_http_node_component() {
        let verdict = screen_builder(
            &[
                "wasi:http/outgoing-handler@0.2.6",
                "wamn:node/credentials@0.1.0",
                "wamn:node/control@0.1.0",
            ],
            "http-node.wasm",
        );
        assert!(
            verdict.is_ok(),
            "http-node must clear the builder lint: {verdict:?}"
        );
    }

    /// The empty `world node` (imports nothing — the frozen minimal world)
    /// clears the builder lint trivially.
    #[test]
    fn builder_lint_passes_empty_world_node() {
        assert!(screen_builder(&[], "node.wasm").is_ok());
    }

    /// The REAL componentized `world node` artifact is not import-free: the
    /// exported `handler` `use`s `wamn:node/types`, so wit-bindgen materializes
    /// a type-only `wamn:node/types` instance import (the in-cluster
    /// sample-node build surfaced this). It must clear the lint AND derive
    /// nothing — types is absent from [`GRANTABLE_HOST_INTERFACES`].
    #[test]
    fn types_only_import_passes_the_lint_and_grants_nothing() {
        let names = ["wamn:node/types@0.1.0"];
        assert!(screen_builder(&names, "sample_node.wasm").is_ok());
        let grants = derive_grants(names);
        assert!(grants.host_interfaces.is_empty());
        assert!(!grants.requires_allowed_hosts);
    }

    /// MUTATION (a) TARGET. A `wasi:sockets` importer is REFUSED by the builder
    /// lint's package arm (arm 1). Removing that arm from
    /// [`screen_builder_compiled`] lets the socket import slip through (arm 2
    /// only screens wamn:node / wasi:http interfaces), flipping this to `Ok`.
    #[test]
    fn builder_lint_refuses_wasi_sockets() {
        let verdict = screen_builder(
            &[
                "wasi:sockets/tcp@0.2.3",
                "wasi:clocks/monotonic-clock@0.2.3",
            ],
            "socket-node.wasm",
        );
        match verdict {
            Err(EgressGuardError::DisallowedTenantImport { component, imports }) => {
                assert_eq!(component, "socket-node.wasm");
                assert_eq!(imports, vec!["wasi:sockets/tcp@0.2.3".to_string()]);
            }
            other => panic!("expected the package arm to refuse wasi:sockets, got {other:?}"),
        }
    }

    /// The builder lint refuses a component that imports a wrong `wasi:http`
    /// interface (`incoming-handler`) via arm 2 — the interface tightening the
    /// package arm cannot express (`wasi:http` IS an allowlisted package).
    #[test]
    fn builder_lint_refuses_wrong_http_interface() {
        let verdict = screen_builder(
            &[
                "wasi:http/incoming-handler@0.2.6",
                "wamn:node/control@0.1.0",
            ],
            "bad-http.wasm",
        );
        match verdict {
            Err(EgressGuardError::DisallowedNodeInterface { component, imports }) => {
                assert_eq!(component, "bad-http.wasm");
                assert_eq!(
                    imports,
                    vec!["wasi:http/incoming-handler@0.2.6".to_string()]
                );
            }
            other => panic!("expected the interface arm to refuse incoming-handler, got {other:?}"),
        }
    }

    /// `derive_grants` (design-note 7): the http-node world derives the three
    /// grantable interfaces AND requires allowedHosts; a bare pure node derives
    /// no grants and requires none. Determinism/std shims are NOT grants.
    #[test]
    fn derive_grants_from_http_node_and_pure_node() {
        let http = derive_grants([
            "wasi:http/outgoing-handler@0.2.6",
            "wamn:node/credentials@0.1.0",
            "wamn:node/control@0.1.0",
            "wasi:clocks/monotonic-clock@0.2.3",
        ]);
        assert!(http.requires_allowed_hosts);
        assert_eq!(
            http.host_interfaces,
            vec![
                "wasi:http/outgoing-handler".to_string(),
                "wamn:node/credentials".to_string(),
                "wamn:node/control".to_string(),
            ]
        );

        let pure = derive_grants(["wasi:clocks/monotonic-clock@0.2.3"]);
        assert!(!pure.requires_allowed_hosts);
        assert!(pure.host_interfaces.is_empty());
    }

    /// The "derived, never declared twice" rule (design-note 7): allowedHosts is
    /// REQUIRED for an http node (refused if empty) and REFUSED for a non-http
    /// node (refused if present); the matching cases pass.
    #[test]
    fn check_allowed_hosts_grant_enforces_required_and_refused() {
        let http = derive_grants(["wasi:http/outgoing-handler@0.2.6"]);
        let pure = derive_grants(["wamn:node/control@0.1.0"]);

        // http node + hosts -> OK; http node + no hosts -> required.
        assert!(check_allowed_hosts_grant(&http, &["api.example:443".to_string()], "n").is_ok());
        assert!(matches!(
            check_allowed_hosts_grant(&http, &[], "n"),
            Err(GrantError::AllowedHostsRequired { .. })
        ));
        // pure node + no hosts -> OK; pure node + hosts -> refused (spurious grant).
        assert!(check_allowed_hosts_grant(&pure, &[], "n").is_ok());
        assert!(matches!(
            check_allowed_hosts_grant(&pure, &["api.example:443".to_string()], "n"),
            Err(GrantError::AllowedHostsRefused { .. })
        ));
    }
}

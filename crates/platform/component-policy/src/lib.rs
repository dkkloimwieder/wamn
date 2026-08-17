//! Structurally refuses publication of first-party components that import
//! `wasi:sockets`.
//!
//! MVP outcome: egress confinement (import allowlist, mutation-proofed).
//!
//! This admission rule rejects every P2 or P3 `wasi:sockets` interface before
//! publication. It is independent of the pinned wasmCloud v2.7.0 runtime
//! policy: `TcpConnect`, `UdpConnect`, and `UdpOutgoingDatagram` deny by default
//! and proceed only with explicit raw-socket opt-in. `UdpBind` remains
//! service-loopback-only, and raw-socket opt-in never widens bind authority.
//! `AllowedIPNameLookups` and the `allowed_hosts` allowlist are independent of
//! that authority; `allowed_hosts` governs `wasi:http` only. See
//! `docs/archive/data-path/security-db-path.md` for the layered boundary and
//! `docs/archive/platform/wash-runtime-fork.md` for the authoritative branch, revision, and
//! carried-policy details.
//!
//! This module is that enforcement: a single structural rule — reject a
//! component that imports any interface of the `wasi:sockets` package — reusable
//! by any first-party wamn build/publish path that has the component bytes. It
//! intentionally keys on the WIT `namespace:package` (`wasi:sockets`), not
//! fragile interface-name matching: every socket interface
//! (`wasi:sockets/tcp@…`, `…/udp@…`, `…/ip-name-lookup@…`, a bare
//! `wasi:sockets@…`) collapses to the same package and is caught by the one
//! rule.

/// Ordered top-level imports declared by a component world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentImports {
    names: Box<[String]>,
}

impl ComponentImports {
    /// Build an import inventory without reordering or normalizing wire names.
    pub fn new(names: impl IntoIterator<Item = String>) -> Self {
        Self {
            names: names.into_iter().collect(),
        }
    }

    /// Iterate over full import names in declaration order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }
}

/// The retained policy population: first-party platform components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyProfile {
    FirstParty,
}

/// A successful policy analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyReport;

/// Analyze one first-party component import inventory.
pub fn analyze(
    imports: &ComponentImports,
    _profile: PolicyProfile,
    label: &str,
) -> Result<PolicyReport, EgressGuardError> {
    screen_imports(imports, label)?;
    Ok(PolicyReport)
}

/// The denied WIT `namespace:package`. A component importing ANY interface of
/// this package can open a raw TCP/UDP socket and reach Postgres directly,
/// bypassing the `wamn:postgres` plugin's tenant-claim / RLS path. This is the
/// single load-bearing literal — pinned by a drift guard in the tests.
pub const DENIED_EGRESS_PKG: &str = "wasi:sockets";

/// Refusal of a component whose world imports a denied egress surface.
#[derive(Debug)]
pub enum EgressGuardError {
    /// The component imports one or more `wasi:sockets` interfaces.
    RawSocketImport {
        /// Caller-supplied label for the offending component.
        component: String,
        /// The full offending import names, e.g. `wasi:sockets/tcp@0.2.3`.
        imports: Vec<String>,
    },
}

impl std::fmt::Display for EgressGuardError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EgressGuardError::RawSocketImport { component, imports } => write!(
                formatter,
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
/// order given. This is the one structural rule; [`screen_imports`] and the
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
pub fn screen_imports(imports: &ComponentImports, label: &str) -> Result<(), EgressGuardError> {
    let denied = denied_imports(imports.iter());
    if denied.is_empty() {
        Ok(())
    } else {
        Err(EgressGuardError::RawSocketImport {
            component: label.to_string(),
            imports: denied,
        })
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

    /// The package rule is ABI-version-independent: publish refuses both the P2
    /// and P3 socket worlds, including P3's consolidated `types` interface.
    #[test]
    fn denied_imports_refuses_p2_and_p3_socket_worlds() {
        let names = [
            "wasi:sockets/tcp@0.2.3",
            "wasi:sockets/types@0.3.0",
            "wasi:sockets/ip-name-lookup@0.3.0",
            "wasi:clocks/monotonic-clock@0.3.0",
        ];
        assert_eq!(
            denied_imports(names),
            vec![
                "wasi:sockets/tcp@0.2.3".to_string(),
                "wasi:sockets/types@0.3.0".to_string(),
                "wasi:sockets/ip-name-lookup@0.3.0".to_string(),
            ]
        );
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

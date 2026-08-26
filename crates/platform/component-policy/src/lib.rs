//! Pure import admission for platform and tenant components.
//!
//! MVP outcome: egress confinement (import allowlist, mutation-proofed).
//!
//! This admission rule rejects every P2 or P3 `wasi:sockets` interface before
//! publication. It is stronger than, and independent of, the pinned vanilla
//! wasmCloud v2.8.0 runtime defense: WAMN installs the centralized
//! `SocketPolicy` with `EgressMode::Enforce`, but tenant artifacts never reach
//! that path because this admission rule rejects the package outright. There
//! is no WAMN raw-socket opt-in. `AllowedIPNameLookups`, `allowed_hosts`, and
//! the vanilla address-range defaults remain runtime policy layers; none grants
//! publication authority. See
//! `docs/architecture/native-alignment-ledger.md` for the authoritative branch,
//! revision, and patch dispositions.
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

use std::collections::BTreeSet;

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

/// The complete WASI package set tenant components may import directly.
///
/// Matching is by exact `namespace:package`; this is deliberately not a
/// `wasi:*` prefix policy. Filesystem, sockets, HTTP, CLI environment/process,
/// and every future WASI package therefore remain denied until this list is
/// changed deliberately.
pub const TENANT_WASI_PACKAGES: [&str; 4] =
    ["wasi:io", "wasi:clocks", "wasi:random", "wasi:logging"];

/// Platform packages that grant a component no authority leaving the host.
///
/// `wamn:node` is the router's own invocation seam — the host calls INTO the
/// guest through it — so importing it reaches nothing outside the host. It is
/// the only platform package with that shape, which is what keeps the
/// complement rule in [`is_effect_package`] fail-safe: a platform package added
/// later is classified as an effect until it is listed here deliberately.
pub const NON_EFFECT_PLATFORM_PACKAGES: [&str; 1] = ["wamn:node"];

/// Whether importing this exact `namespace:package` reaches outside the host.
///
/// This is the complement of the two authority-free lists over an inventory
/// [`analyze_tenant`] has already accepted: every package left is an admitted
/// platform capability, and an admitted platform capability is an effect
/// surface unless [`NON_EFFECT_PLATFORM_PACKAGES`] says otherwise.
pub fn is_effect_package(package: &str) -> bool {
    !TENANT_WASI_PACKAGES.contains(&package) && !NON_EFFECT_PLATFORM_PACKAGES.contains(&package)
}

/// Stable classification for a refused tenant import inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenantImportErrorKind {
    InvalidPlatformCapability,
    UnadmittedImport,
}

/// Refusal from the closed tenant import policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantImportError {
    kind: TenantImportErrorKind,
    component: Box<str>,
    imports: Box<[String]>,
}

impl TenantImportError {
    /// Stable refusal class for callers that must not match display text.
    pub fn kind(&self) -> TenantImportErrorKind {
        self.kind
    }

    /// Exact registry entry or component imports that were refused.
    pub fn imports(&self) -> &[String] {
        &self.imports
    }
}

impl std::fmt::Display for TenantImportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            TenantImportErrorKind::InvalidPlatformCapability => write!(
                formatter,
                "component {:?} was given invalid platform capability package(s) {:?}",
                self.component, self.imports
            ),
            TenantImportErrorKind::UnadmittedImport => write!(
                formatter,
                "component {:?} imports unadmitted package interface(s) {:?}",
                self.component, self.imports
            ),
        }
    }
}

impl std::error::Error for TenantImportError {}

/// Enforce the closed tenant import policy.
///
/// `admitted_platform_packages` is the caller's closed platform registry
/// projection for this component. Its entries must be exact unversioned
/// `wamn:<package>` names. An import is accepted only when its package is one
/// of those exact entries or one of [`TENANT_WASI_PACKAGES`].
pub fn analyze_tenant(
    imports: &ComponentImports,
    admitted_platform_packages: &BTreeSet<String>,
    label: &str,
) -> Result<PolicyReport, TenantImportError> {
    let invalid_registry: Vec<_> = admitted_platform_packages
        .iter()
        .filter(|package| !valid_platform_package(package))
        .cloned()
        .collect();
    if !invalid_registry.is_empty() {
        return Err(TenantImportError {
            kind: TenantImportErrorKind::InvalidPlatformCapability,
            component: label.into(),
            imports: invalid_registry.into_boxed_slice(),
        });
    }

    let denied: Vec<_> = imports
        .iter()
        .filter(|name| {
            let package = import_pkg(name);
            !TENANT_WASI_PACKAGES.contains(&package)
                && !admitted_platform_packages.contains(package)
        })
        .map(str::to_owned)
        .collect();
    if denied.is_empty() {
        Ok(PolicyReport)
    } else {
        Err(TenantImportError {
            kind: TenantImportErrorKind::UnadmittedImport,
            component: label.into(),
            imports: denied.into_boxed_slice(),
        })
    }
}

fn valid_platform_package(package: &str) -> bool {
    package
        .strip_prefix("wamn:")
        .is_some_and(|name| !name.is_empty() && !name.contains(['/', '@', '*']))
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
pub fn import_pkg(import_name: &str) -> &str {
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

    #[test]
    fn tenant_policy_is_exact_and_has_no_namespace_wildcard() {
        let admitted = BTreeSet::from(["wamn:postgres".to_string()]);
        let imports = ComponentImports::new([
            "wasi:io/streams@0.2.3".to_string(),
            "wasi:clocks/monotonic-clock@0.2.3".to_string(),
            "wasi:random/random@0.2.3".to_string(),
            "wasi:logging/logging@0.1.0-draft".to_string(),
            "wamn:postgres/client@0.1.0".to_string(),
        ]);

        assert!(analyze_tenant(&imports, &admitted, "tenant-node").is_ok());
    }

    #[test]
    fn tenant_policy_refuses_every_unlisted_authority_surface() {
        let imports = ComponentImports::new([
            "wasi:filesystem/types@0.2.3".to_string(),
            "wasi:sockets/tcp@0.2.3".to_string(),
            "wasi:http/outgoing-handler@0.2.3".to_string(),
            "wasi:cli/environment@0.2.3".to_string(),
            "wamn:postgres/client@0.1.0".to_string(),
            "other:host/process@1.0.0".to_string(),
        ]);

        let error = analyze_tenant(&imports, &BTreeSet::new(), "tenant-node")
            .expect_err("unlisted packages must refuse");
        assert_eq!(error.kind(), TenantImportErrorKind::UnadmittedImport);
        assert_eq!(error.imports().len(), 6);
    }

    /// The whole admitted vocabulary, classified. Every package the tenant
    /// policy can accept is either authority-free or an effect, and an
    /// unrecognized platform package falls to the effect side rather than
    /// disappearing from a component's recorded authority.
    #[test]
    fn effect_classification_is_the_complement_of_the_authority_free_lists() {
        for authority_free in TENANT_WASI_PACKAGES
            .iter()
            .chain(NON_EFFECT_PLATFORM_PACKAGES.iter())
        {
            assert!(
                !is_effect_package(authority_free),
                "{authority_free:?} must not be recorded as an effect"
            );
        }
        for effect in [
            "wamn:postgres",
            "wamn:connection",
            "wamn:jetstream",
            "wamn:not-yet-invented",
        ] {
            assert!(
                is_effect_package(effect),
                "{effect:?} must be recorded as an effect"
            );
        }
    }

    #[test]
    fn tenant_platform_registry_rejects_wildcards_and_interface_names() {
        for entry in ["wamn:*", "wamn:postgres/client", "wamn:postgres@0.1.0"] {
            let admitted = BTreeSet::from([entry.to_string()]);
            let error =
                analyze_tenant(&ComponentImports::new(Vec::new()), &admitted, "tenant-node")
                    .expect_err("the platform registry must contain exact package names");
            assert_eq!(
                error.kind(),
                TenantImportErrorKind::InvalidPlatformCapability
            );
        }
    }
}

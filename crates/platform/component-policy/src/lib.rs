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

/// Whether importing a capability reaches outside the host.
///
/// Posture is package-grain and **declared**, never inferred from a namespace.
/// A mixed-posture package would trigger a split ruling; none exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posture {
    /// Reaches nothing outside the host. Importable without a grant.
    Ambient,
    /// Reaches outside the host. Requires an explicit per-component grant and
    /// is recorded in the component's effect projection.
    Effect,
}

/// One admitted capability, keyed on exact `namespace:package` AND exact
/// version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityRow {
    /// Exact WIT `namespace:package`.
    pub package: &'static str,
    /// Exact package version. No normalization, no ranges.
    pub version: &'static str,
    /// Whether importing it leaves the host.
    pub posture: Posture,
}

/// The closed capability registry: the complete set of packages a tenant
/// component may import, each with its declared posture.
///
/// This replaces two implicit classifiers — a `wamn:`-prefix shape check and a
/// namespace complement rule — with one declared table. Three properties are
/// deliberate:
///
/// * **Closed, and fail-closed.** An import with no exact row here is refused
///   by ABSENCE, not by a namespace opinion. Expansion is a ruled change.
/// * **Exact version.** Admission requires the imported version to equal the
///   row's version. No normalization, no ranges; compatible-range rules are
///   lifecycle machinery parked on `.7`.
/// * **Code, not catalog.** Posture is a security classification that must be
///   byte-identical on every host and must move only through code review plus
///   a ledger row. A database row would let an operator reclassify an effect
///   as ambient.
///
/// The registry governs the TENANT path ([`analyze_tenant`]) only. `wash push`
/// workloads are platform-authored and admitted by us, so their imports are the
/// platform's own responsibility; that path re-converges at the first
/// non-platform-authored pushed workload.
///
/// Rows record what the host **offers**, not what a guest currently imports.
/// `wasi:logging` is host-implemented (`plugins/wamn_logging.rs`) with zero
/// importers today: a registry keyed on imports would drop a live capability
/// the moment its last importer churned, then refuse the next guest to want it.
///
/// # Two provenance classes
///
/// The `wamn:*` rows and `wasi:logging` carry versions WE author. The remaining
/// `wasi:*` rows do not: measured on `receiving`, the authored WIT says
/// `0.2.12`, the raw build imports `0.2.9`, and the virtualized artifact that
/// admission actually sees imports `0.2.12` — the virtualizer rewrites it. Those
/// rows are pinned to the WASI-Virt revision and adapter digest of
/// `docs/architecture/native-alignment-ledger.md` row 5, and a bump there
/// without a matching edit here would silently refuse every std guest.
/// `capability_registry_wasi_rows_match_the_vendored_wit` in the conformance
/// suite is what makes that fail at the gate instead.
pub const CAPABILITY_REGISTRY: [CapabilityRow; 8] = [
    // Versions we author.
    CapabilityRow {
        package: "wamn:node",
        version: "0.1.0",
        posture: Posture::Ambient,
    },
    CapabilityRow {
        package: "wamn:postgres",
        version: "0.1.0",
        posture: Posture::Effect,
    },
    CapabilityRow {
        package: "wamn:connection",
        version: "0.1.0",
        posture: Posture::Effect,
    },
    CapabilityRow {
        package: "wasmcloud:blobstore",
        version: "0.1.0",
        posture: Posture::Effect,
    },
    CapabilityRow {
        package: "wasi:logging",
        version: "0.1.0-draft",
        posture: Posture::Ambient,
    },
    // Versions the virtualizer authors. See the provenance note above.
    CapabilityRow {
        package: "wasi:io",
        version: "0.2.12",
        posture: Posture::Ambient,
    },
    CapabilityRow {
        package: "wasi:clocks",
        version: "0.2.12",
        posture: Posture::Ambient,
    },
    CapabilityRow {
        package: "wasi:random",
        version: "0.2.12",
        posture: Posture::Ambient,
    },
];

/// The registry row for one full import name, matched on package AND version.
///
/// `None` means refuse: either the package is unregistered, or it is registered
/// at a different version, or the import carries no version at all.
pub fn capability_row(import_name: &str) -> Option<&'static CapabilityRow> {
    let package = import_pkg(import_name);
    let version = import_version(import_name)?;
    CAPABILITY_REGISTRY
        .iter()
        .find(|row| row.package == package && row.version == version)
}

/// The declared posture of one full import name, or `None` if unregistered.
pub fn import_posture(import_name: &str) -> Option<Posture> {
    capability_row(import_name).map(|row| row.posture)
}

/// The exact version of a full import name.
///
/// Import names carry the version last in both shapes an inventory contains —
/// `wamn:postgres/client@0.1.0` (an interface) and `wasi:clocks@0.2.12` (a bare
/// package) — so the last `@` separates it. `None` for an unversioned name,
/// which the registry then refuses.
pub fn import_version(import_name: &str) -> Option<&str> {
    import_name
        .rsplit_once('@')
        .map(|(_, version)| version)
        .filter(|version| !version.is_empty())
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
/// `admitted_platform_packages` is the caller's per-component grant: exact
/// unversioned package names, each of which must appear in
/// [`CAPABILITY_REGISTRY`]. An import is accepted when it matches a registry
/// row on package AND version, and — if that row is [`Posture::Effect`] — when
/// its package is also in the grant. Ambient rows need no grant.
pub fn analyze_tenant(
    imports: &ComponentImports,
    admitted_platform_packages: &BTreeSet<String>,
    label: &str,
) -> Result<PolicyReport, TenantImportError> {
    // A grant may only name a registered package. This replaces the old
    // `wamn:`-prefix shape check: the registry, not a namespace rule, decides
    // what a grant is allowed to say.
    let invalid_registry: Vec<_> = admitted_platform_packages
        .iter()
        .filter(|package| {
            !CAPABILITY_REGISTRY
                .iter()
                .any(|row| row.package == *package)
        })
        .cloned()
        .collect();
    if !invalid_registry.is_empty() {
        return Err(TenantImportError {
            kind: TenantImportErrorKind::InvalidPlatformCapability,
            component: label.into(),
            imports: invalid_registry.into_boxed_slice(),
        });
    }

    // Fail-closed by ABSENCE: an import with no exact row is refused because
    // nothing declares it, not because its namespace looks wrong. An effect
    // additionally needs the per-component grant — deleting the shape check
    // widens no one's grant, because the two layers stay separate.
    let denied: Vec<_> = imports
        .iter()
        .filter(|name| match import_posture(name) {
            None => true,
            Some(Posture::Ambient) => false,
            Some(Posture::Effect) => !admitted_platform_packages.contains(import_pkg(name)),
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

    /// The whole registered vocabulary at its REGISTERED versions passes.
    ///
    /// These versions are load-bearing, not decoration: this fixture read
    /// `@0.2.3` before the registry landed, and exact matching correctly
    /// refuses that now. A WASI version bump breaks this test, which is the
    /// point — see the provenance note on [`CAPABILITY_REGISTRY`].
    #[test]
    fn tenant_policy_is_exact_and_has_no_namespace_wildcard() {
        let admitted = BTreeSet::from(["wamn:postgres".to_string()]);
        let imports = ComponentImports::new([
            "wasi:io/streams@0.2.12".to_string(),
            "wasi:clocks/monotonic-clock@0.2.12".to_string(),
            "wasi:random/random@0.2.12".to_string(),
            "wasi:logging/logging@0.1.0-draft".to_string(),
            "wamn:postgres/client@0.1.0".to_string(),
            "wamn:node/types@0.1.0".to_string(),
        ]);

        assert!(analyze_tenant(&imports, &admitted, "tenant-node").is_ok());

        // Same inventory, one version off: refused.
        let drifted = ComponentImports::new(["wasi:io/streams@0.2.9".to_string()]);
        let error = analyze_tenant(&drifted, &admitted, "tenant-node")
            .expect_err("a drifted WASI version must refuse");
        assert_eq!(error.kind(), TenantImportErrorKind::UnadmittedImport);
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

    /// Posture is DECLARED, not inferred. The old complement rule classified an
    /// unknown `wamn:*` package as an effect; the registry refuses it outright,
    /// which is the whole behaviour change.
    #[test]
    fn posture_is_declared_and_an_unregistered_package_has_none() {
        assert_eq!(
            import_posture("wamn:node/types@0.1.0"),
            Some(Posture::Ambient)
        );
        assert_eq!(
            import_posture("wasi:logging/logging@0.1.0-draft"),
            Some(Posture::Ambient)
        );
        assert_eq!(
            import_posture("wamn:postgres/client@0.1.0"),
            Some(Posture::Effect)
        );
        assert_eq!(
            import_posture("wasmcloud:blobstore/blobstore@0.1.0"),
            Some(Posture::Effect)
        );
        for unregistered in [
            "wamn:jetstream/consumer@0.1.0",
            "wamn:not-yet-invented/thing@0.1.0",
            "wasi:filesystem/types@0.2.12",
        ] {
            assert_eq!(
                import_posture(unregistered),
                None,
                "{unregistered:?} must be refused by absence, not classified"
            );
        }
    }

    /// Exact version, no ranges. The registry carries `wasi:io@0.2.12`; the
    /// same package at any other version is a different capability and is
    /// refused. This is the mutant target for the version half of the key.
    #[test]
    fn version_matching_is_exact() {
        assert_eq!(
            import_posture("wasi:io/streams@0.2.12"),
            Some(Posture::Ambient)
        );
        for wrong in [
            "wasi:io/streams@0.2.9",
            "wasi:io/streams@0.2.13",
            "wasi:io/streams@0.2",
            "wasi:io/streams",
        ] {
            assert_eq!(
                import_posture(wrong),
                None,
                "{wrong:?} must not match the registered 0.2.12 row"
            );
        }
        assert_eq!(import_version("wamn:postgres/client@0.1.0"), Some("0.1.0"));
        assert_eq!(import_version("wasi:clocks@0.2.12"), Some("0.2.12"));
        assert_eq!(
            import_version("wasi:logging/logging@0.1.0-draft"),
            Some("0.1.0-draft")
        );
        assert_eq!(import_version("wasi:clocks"), None);
    }

    /// The registry is a closed set with unique keys; a duplicate row would let
    /// one package carry two postures and make the lookup order-dependent.
    #[test]
    fn registry_keys_are_unique() {
        let mut keys: Vec<(&str, &str)> = CAPABILITY_REGISTRY
            .iter()
            .map(|row| (row.package, row.version))
            .collect();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total, "duplicate registry key");
        let mut packages: Vec<&str> = CAPABILITY_REGISTRY.iter().map(|row| row.package).collect();
        packages.sort_unstable();
        packages.dedup();
        assert_eq!(
            packages.len(),
            total,
            "a package appears at two versions; posture is package-grain"
        );
    }

    /// An effect still needs its per-component grant. Deleting the shape check
    /// must not widen anyone's grant — this is the test that says so.
    #[test]
    fn an_effect_without_a_grant_is_refused_but_an_ambient_is_not() {
        let effect = ComponentImports::new(["wamn:postgres/client@0.1.0".to_string()]);
        let error = analyze_tenant(&effect, &BTreeSet::new(), "tenant-node")
            .expect_err("an ungranted effect must refuse");
        assert_eq!(error.kind(), TenantImportErrorKind::UnadmittedImport);

        let granted = BTreeSet::from(["wamn:postgres".to_string()]);
        assert!(analyze_tenant(&effect, &granted, "tenant-node").is_ok());

        let ambient = ComponentImports::new(["wamn:node/types@0.1.0".to_string()]);
        assert!(
            analyze_tenant(&ambient, &BTreeSet::new(), "tenant-node").is_ok(),
            "an ambient row needs no grant"
        );
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

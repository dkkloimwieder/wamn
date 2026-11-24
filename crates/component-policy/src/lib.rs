//! Pure import admission for platform and project components.
//!
//! MVP outcome: egress confinement (import allowlist, tested with mutations).
//!
//! Application admission rejects every P2 or P3 `wasi:sockets` interface before
//! publication. WAMN installs the runtime's `SocketPolicy` with
//! `EgressMode::Enforce`, which blocks traffic refused by policy.
//! Project artifacts never reach that path because admission rejects the
//! package outright. The integration profile admits pinned sockets and refuses
//! every other effect. Integration stores enforce their bound authority.
//! `AllowedIPNameLookups`, `allowed_hosts`, and
//! the vanilla address-range defaults remain runtime policy layers; none grants
//! publication authority. See
//! `docs/architecture/native-alignment.md` for current upstream ownership
//! and retained differences.
//!
//! This module is that enforcement: a single structural rule — reject a
//! component that imports any interface of the `wasi:sockets` package — reusable
//! by any first-party wamn build/publish path that has the component bytes. It
//! intentionally keys on the WIT `namespace:package` (`wasi:sockets`), not
//! fragile interface-name matching: every socket interface
//! (`wasi:sockets/tcp@…`, `…/udp@…`, `…/ip-name-lookup@…`, a bare
//! `wasi:sockets@…`) collapses to the same package and is caught by the one
//! rule.

mod application_member;
mod component_facts;

pub use application_member::validate_application_member;
mod signed_file_read;

pub use component_facts::{
    AdmittedComponent, AdmittedComponentEffect, AdmittedComponentFacts, AdmittedComponentOperation,
    AdmittedComponentParameter, AdmittedComponentPort, ComponentDescriptor,
    ComponentEffectProvenance, ComponentFactError, ComponentFactErrorType,
    ComponentPackageCoordinate, ComponentSchema, normalize_component_fact, split_target,
    validate_committed_result_schema, verify_stored_effect_projection,
};
pub use signed_file_read::signed_file_reads;

/// The package document with the descriptor publication writes.
pub type PackageManifest = wamn_package::manifest::PackageManifest<ComponentDescriptor>;

/// Ordered top-level imports declared by a component world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentImports {
    names: Box<[String]>,
}

impl ComponentImports {
    /// Build an import list without reordering or normalizing wire names.
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

/// Component declaration plus its exact admitted platform imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentAdmissionRequest {
    pub declaration: wamn_package::ComponentDeclaration,
    pub admitted_platform_packages: BTreeSet<String>,
    /// The component digest of each base the package manifest pins in
    /// `base_dependencies`. A composed component embeds only these bases.
    pub base_digests: BTreeSet<wamn_digest::Digest>,
}

/// Validates exact component bytes against their declaration and import policy.
pub trait ComponentAdmission: Send + Sync {
    /// Return the descriptor sealed against the supplied bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if validation cannot start or the component is refused.
    fn admit(
        &self,
        bytes: &[u8],
        request: ComponentAdmissionRequest,
    ) -> anyhow::Result<ComponentDescriptor>;
}

/// The interface that each member of a composed component imports as an
/// instance of its own
/// (docs/architecture/components.md#grouping-and-publication).
pub const STATEMENTS_INTERFACE: &str = "wamn:postgres/statements@0.4.0";

/// The interface instance type of one component import, which policy admits.
///
/// An import named for its interface (`wamn:postgres/statements@0.4.0`) is an
/// instance of that interface. A labelled import (`sql-0`) names its interface
/// in its `(implements ..)` annotation, so two instances of one interface are
/// covered by the same platform import row.
pub fn instance_type<'a>(name: &'a str, implements: Option<&'a str>) -> &'a str {
    implements.unwrap_or(name)
}

/// The production admission profiles for platform components and integrations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyProfile {
    FirstParty,
    Integration,
}

/// A successful policy analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyReport;

/// Analyze imports under the selected production admission profile.
pub fn analyze(
    imports: &ComponentImports,
    profile: PolicyProfile,
    label: &str,
) -> Result<PolicyReport, EgressGuardError> {
    if profile == PolicyProfile::Integration {
        let denied = imports
            .iter()
            .filter(|name| !integration_import(name))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !denied.is_empty() {
            return Err(EgressGuardError::IntegrationImport {
                component: label.to_owned(),
                imports: denied,
            });
        }
    } else {
        screen_imports(imports, label)?;
    }
    Ok(PolicyReport)
}

fn integration_import(name: &str) -> bool {
    match (import_pkg(name), import_version(name)) {
        ("wasi:sockets" | "wasi:io" | "wasi:clocks" | "wasi:random", Some(version)) => {
            admits_version(WASI_VERSION, version)
        }
        ("wasi:cli", _) => platform_import_row(name).is_some(),
        _ => name == "wamn:integration/bound@0.1.0",
    }
}

/// The admitted version of the standard WASI imports.
pub const WASI_VERSION: &str = "0.2.12";

/// Whether a row's exact version matches an import's version.
fn admits_version(row_version: &str, version: &str) -> bool {
    row_version == version
}

/// Whether a platform import reaches outside the host.
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

/// One admitted import, keyed on exact `namespace:package` and version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformImportRow {
    /// Exact WIT `namespace:package`.
    pub package: &'static str,
    /// Exact package version.
    pub version: &'static str,
    /// Whether importing it leaves the host.
    pub posture: Posture,
}

/// The closed platform import registry: the complete set of packages a project
/// component may import, each with its declared posture.
///
/// This replaces two implicit classifiers — a `wamn:`-prefix shape test and a
/// namespace complement rule — with one declared table. Three properties are
/// deliberate:
///
/// * **Closed, and fail-closed.** An import with no matching row here is
///   refused by ABSENCE, not by a namespace opinion. Expansion is a ruled
///   change.
/// * **Exact version.** Each row admits only its declared package version.
/// * **Code, not catalog.** Posture is a security classification that must be
///   byte-identical on every host and must move only through code review plus
///   a stored row. A database row would let an operator reclassify an effect
///   as ambient.
///
/// The registry governs the PROJECT path ([`analyze_project`]) only. `wash push`
/// workloads are platform-authored and admitted by us, so their imports are the
/// platform's own responsibility; that path re-converges at the first
/// non-platform-authored pushed workload.
///
/// Rows record what the host **offers**, not what a guest currently imports.
/// `wasi:logging` is host-implemented by wash-runtime with zero
/// importers today: a registry keyed on imports would drop an offered interface
/// the moment its last importer churned, then refuse the next guest to want it.
///
/// # Two provenance classes
///
/// The `wamn:*` rows and `wasi:logging` carry versions we author. Standard
/// WASI imports come from the Rust toolchain. Their rows admit the host's
/// supported version without rewriting the component bytes.
pub const PLATFORM_IMPORT_REGISTRY: [PlatformImportRow; 12] = [
    // Versions we author.
    PlatformImportRow {
        package: "wamn:modbus",
        version: "1.0.0",
        posture: Posture::Effect,
    },
    PlatformImportRow {
        package: "wamn:integration",
        version: "0.1.0",
        posture: Posture::Effect,
    },
    PlatformImportRow {
        package: "wamn:node",
        version: "0.2.0",
        posture: Posture::Ambient,
    },
    PlatformImportRow {
        package: "wamn:postgres",
        version: "0.1.0",
        posture: Posture::Effect,
    },
    // 0.4.0 is 0.3.0 without the participant calls of `statements`. 0.3.0
    // added `statements.run-stream` and `statements.operation-transaction`.
    PlatformImportRow {
        package: "wamn:postgres",
        version: "0.4.0",
        posture: Posture::Effect,
    },
    PlatformImportRow {
        package: "wamn:connection",
        version: "0.1.0",
        posture: Posture::Effect,
    },
    PlatformImportRow {
        package: "wasmcloud:blobstore",
        version: "0.1.0",
        posture: Posture::Effect,
    },
    PlatformImportRow {
        package: "wasi:logging",
        version: "0.1.0-draft",
        posture: Posture::Ambient,
    },
    // Standard WASI version families. See the provenance note
    // above.
    PlatformImportRow {
        package: "wasi:io",
        version: WASI_VERSION,
        posture: Posture::Ambient,
    },
    PlatformImportRow {
        package: "wasi:clocks",
        version: WASI_VERSION,
        posture: Posture::Ambient,
    },
    PlatformImportRow {
        package: "wasi:cli",
        version: WASI_VERSION,
        posture: Posture::Ambient,
    },
    PlatformImportRow {
        package: "wasi:random",
        version: WASI_VERSION,
        posture: Posture::Ambient,
    },
];

/// Ambient CLI interfaces supplied with empty arguments, environment and streams.
pub const AMBIENT_CLI_INTERFACES: [&str; 10] = [
    "wasi:cli/environment",
    "wasi:cli/exit",
    "wasi:cli/stdin",
    "wasi:cli/stdout",
    "wasi:cli/stderr",
    "wasi:cli/terminal-input",
    "wasi:cli/terminal-output",
    "wasi:cli/terminal-stdin",
    "wasi:cli/terminal-stdout",
    "wasi:cli/terminal-stderr",
];

/// Interfaces an application component may not import, whatever its grant,
/// matched on the import's interface instance type at every version.
pub const APPLICATION_DENIED_INTERFACES: [&str; 1] = ["wamn:integration/bound"];

/// Whether an application component may not import this interface instance type.
fn is_denied_to_applications(import_name: &str) -> bool {
    let interface = import_name
        .rsplit_once('@')
        .map_or(import_name, |(interface, _)| interface);
    APPLICATION_DENIED_INTERFACES.contains(&interface)
}

/// The registry row for one full import name, matched on package AND version.
///
/// `None` means refuse: either the package is unregistered, or no row of it
/// admits the version, or the import carries no version at all.
pub fn platform_import_row(import_name: &str) -> Option<&'static PlatformImportRow> {
    let package = import_pkg(import_name);
    let version = import_version(import_name)?;
    if package == "wasi:cli" && !AMBIENT_CLI_INTERFACES.contains(&import_name.rsplit_once('@')?.0) {
        return None;
    }
    PLATFORM_IMPORT_REGISTRY
        .iter()
        .find(|row| row.package == package && admits_version(row.version, version))
}

/// Whether this import's package is registered, at any version.
///
/// Distinct from [`import_posture`], which matches package AND version. Some
/// callers need to know what TYPE of thing an import is — a platform
/// import versus a cross-package application call — and that type does not
/// change with the version. A `wasi:clocks` import at an unregistered version
/// is still a platform import; it is admission's job to refuse the
/// version, not this classifier's job to reclassify it as an application call.
pub fn is_registered_package(import_name: &str) -> bool {
    let package = import_pkg(import_name);
    PLATFORM_IMPORT_REGISTRY
        .iter()
        .any(|row| row.package == package)
}

/// The declared posture of one full import name, or `None` if unregistered.
pub fn import_posture(import_name: &str) -> Option<Posture> {
    platform_import_row(import_name).map(|row| row.posture)
}

/// The exact version of a full import name.
///
/// Import names carry the version last in both shapes an import list contains —
/// `wamn:postgres/client@0.1.0` (an interface) and `wasi:clocks@0.2.12` (a bare
/// package) — so the last `@` separates it. `None` for an unversioned name,
/// which the registry then refuses.
pub fn import_version(import_name: &str) -> Option<&str> {
    import_name
        .rsplit_once('@')
        .map(|(_, version)| version)
        .filter(|version| !version.is_empty())
}

/// Stable classification for a refused project import list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectImportErrorType {
    InvalidPlatformImport,
    UnadmittedImport,
}

/// Refusal from the closed project import policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectImportError {
    type_: ProjectImportErrorType,
    component: Box<str>,
    imports: Box<[String]>,
}

impl ProjectImportError {
    /// Stable refusal class for callers that must not match display text.
    pub fn error_type(&self) -> ProjectImportErrorType {
        self.type_
    }

    /// Exact registry entry or component imports that were refused.
    pub fn imports(&self) -> &[String] {
        &self.imports
    }
}

impl std::fmt::Display for ProjectImportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.type_ {
            ProjectImportErrorType::InvalidPlatformImport => write!(
                formatter,
                "component {:?} was given invalid platform import package(s) {:?}",
                self.component, self.imports
            ),
            ProjectImportErrorType::UnadmittedImport => write!(
                formatter,
                "component {:?} imports unadmitted package interface(s) {:?}",
                self.component, self.imports
            ),
        }
    }
}

impl std::error::Error for ProjectImportError {}

/// Enforce the closed project import policy.
///
/// `admitted_platform_packages` is the caller's per-component grant: exact
/// unversioned package names, each of which must appear in
/// [`PLATFORM_IMPORT_REGISTRY`]. An import is accepted when it matches a registry
/// row on package AND version, and — if that row is [`Posture::Effect`] — when
/// its package is also in the grant. Ambient rows need no grant. An interface
/// in [`APPLICATION_DENIED_INTERFACES`] is refused even with its grant.
pub fn analyze_project(
    imports: &ComponentImports,
    admitted_platform_packages: &BTreeSet<String>,
    label: &str,
) -> Result<PolicyReport, ProjectImportError> {
    // A grant may only name a registered package. This replaces the old
    // `wamn:`-prefix shape condition: the registry, not a namespace rule, decides
    // what a grant is allowed to say.
    let invalid_registry: Vec<_> = admitted_platform_packages
        .iter()
        .filter(|package| {
            !PLATFORM_IMPORT_REGISTRY
                .iter()
                .any(|row| row.package == *package)
        })
        .cloned()
        .collect();
    if !invalid_registry.is_empty() {
        return Err(ProjectImportError {
            type_: ProjectImportErrorType::InvalidPlatformImport,
            component: label.into(),
            imports: invalid_registry.into_boxed_slice(),
        });
    }

    // Fail-closed by ABSENCE: an import with no matching row is refused because
    // nothing declares it, not because its namespace looks wrong. An effect
    // additionally needs the per-component grant — deleting the shape test
    // widens no one's grant, because the two layers stay separate.
    let denied: Vec<_> = imports
        .iter()
        .filter(|name| match import_posture(name) {
            _ if is_denied_to_applications(name) => true,
            None => true,
            Some(Posture::Ambient) => false,
            Some(Posture::Effect) => !admitted_platform_packages.contains(import_pkg(name)),
        })
        .map(str::to_owned)
        .collect();
    if denied.is_empty() {
        Ok(PolicyReport)
    } else {
        Err(ProjectImportError {
            type_: ProjectImportErrorType::UnadmittedImport,
            component: label.into(),
            imports: denied.into_boxed_slice(),
        })
    }
}

/// The denied WIT `namespace:package`. A component importing ANY interface of
/// this package can open a raw TCP/UDP socket and reach Postgres directly,
/// bypassing the `wamn:postgres` plugin's project-identity / RLS path. This is the
/// single load-bearing literal — pinned by a drift guard in the tests.
pub const DENIED_EGRESS_PKG: &str = "wasi:sockets";

/// Refusal of a component whose world imports a denied egress surface.
#[derive(Debug)]
pub enum EgressGuardError {
    /// An integration imports an effect outside its bound sockets.
    IntegrationImport {
        component: String,
        imports: Vec<String>,
    },
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
            EgressGuardError::IntegrationImport { component, imports } => write!(
                formatter,
                "integration {component:?} imports unadmitted interface(s) {imports:?}"
            ),
            EgressGuardError::RawSocketImport { component, imports } => write!(
                formatter,
                "component {component:?} imports raw-socket interface(s) {imports:?} \
                 (package {DENIED_EGRESS_PKG:?}) — this opens arbitrary outbound TCP with DNS, \
                 bypassing the wamn:postgres project-identity / RLS path; the platform refuses to \
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
/// condition both go through it. Empty result == the component clears the guard.
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

    #[test]
    fn integration_profile_grants_only_bound_sockets_and_ambient_imports() {
        let imports = ComponentImports::new(
            [
                "wamn:integration/bound@0.1.0",
                "wasi:sockets/tcp@0.2.12",
                "wasi:io/streams@0.2.12",
            ]
            .map(str::to_owned),
        );
        analyze(&imports, PolicyProfile::Integration, "integration").expect("bound sockets");
        analyze_project(
            &imports,
            &BTreeSet::from(["wamn:integration".to_owned()]),
            "application",
        )
        .expect_err("application has no sockets or binding");
        for effect in [
            "wamn:postgres/statements@0.4.0",
            "wasmcloud:blobstore/blobstore@0.1.0",
            "wasi:http/outgoing-handler@0.2.12",
            "wasi:sockets/tcp@0.3.0",
            "wamn:modbus/read@1.0.0",
        ] {
            analyze(
                &ComponentImports::new([effect.to_owned()]),
                PolicyProfile::Integration,
                "integration",
            )
            .expect_err("unlisted effect");
        }
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

    /// A standard workload — the DB plugin, the `allowed_hosts`-conditioned http
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
    /// These versions are load-bearing, not decoration: an authored version
    /// one off is refused. See the provenance note on [`PLATFORM_IMPORT_REGISTRY`].
    #[test]
    fn project_policy_is_exact_and_has_no_namespace_wildcard() {
        let admitted = BTreeSet::from(["wamn:postgres".to_string()]);
        let imports = ComponentImports::new([
            "wasi:io/streams@0.2.12".to_string(),
            "wasi:clocks/monotonic-clock@0.2.12".to_string(),
            "wasi:random/random@0.2.12".to_string(),
            "wasi:logging/logging@0.1.0-draft".to_string(),
            "wamn:postgres/statements@0.4.0".to_string(),
            "wamn:node/types@0.2.0".to_string(),
        ]);

        assert!(analyze_project(&imports, &admitted, "project-node").is_ok());

        let ambient = ComponentImports::new(
            super::AMBIENT_CLI_INTERFACES
                .iter()
                .map(|interface| format!("{interface}@0.2.12")),
        );
        assert!(analyze_project(&ambient, &BTreeSet::new(), "project-node").is_ok());
        for version in ["0.2.9", "0.2.13", "0.2.4294967296", "0.2.12-rc.1"] {
            let import = format!("wasi:io/streams@{version}");
            assert!(platform_import_row(&import).is_none());
            assert!(!integration_import(&import));
        }
        assert!(integration_import("wasi:io/streams@0.2.12"));
        let drifted = ComponentImports::new(["wasi:cli/environment@0.3.0".to_owned()]);
        assert!(analyze_project(&drifted, &BTreeSet::new(), "project-node").is_err());

        // Same import list, one authored version off: refused.
        let drifted = ComponentImports::new(["wamn:node/types@0.1.0".to_string()]);
        let error = analyze_project(&drifted, &admitted, "project-node")
            .expect_err("a drifted authored version must refuse");
        assert_eq!(error.error_type(), ProjectImportErrorType::UnadmittedImport);
    }

    #[test]
    fn project_policy_refuses_every_unlisted_authority_surface() {
        let imports = ComponentImports::new([
            format!("wasi:filesystem/types@{WASI_VERSION}"),
            format!("wasi:sockets/tcp@{WASI_VERSION}"),
            format!("wasi:http/outgoing-handler@{WASI_VERSION}"),
            format!("wasi:cli/run@{WASI_VERSION}"),
            "wamn:postgres/client@0.1.0".to_string(),
            "other:host/process@1.0.0".to_string(),
        ]);

        for import in imports.iter() {
            let error = analyze_project(
                &ComponentImports::new([import.to_owned()]),
                &BTreeSet::new(),
                "project-node",
            )
            .expect_err("unlisted authority must refuse");
            assert_eq!(error.error_type(), ProjectImportErrorType::UnadmittedImport);
        }
    }

    /// Posture is DECLARED, not inferred. The old complement rule classified an
    /// unknown `wamn:*` package as an effect; the registry refuses it outright,
    /// which is the whole behaviour change.
    #[test]
    fn posture_is_declared_and_an_unregistered_package_has_none() {
        assert_eq!(
            import_posture("wamn:node/types@0.2.0"),
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

    /// The registry is a closed set with unique keys; a duplicate row would let
    /// one package carry two postures and make the lookup order-dependent.
    #[test]
    fn registry_keys_are_unique() {
        let mut keys: Vec<(&str, &str)> = PLATFORM_IMPORT_REGISTRY
            .iter()
            .map(|row| (row.package, row.version))
            .collect();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total, "duplicate registry key");
        for row in &PLATFORM_IMPORT_REGISTRY {
            assert!(
                PLATFORM_IMPORT_REGISTRY
                    .iter()
                    .filter(|other| other.package == row.package)
                    .all(|other| other.posture == row.posture),
                "{} has two postures; posture is package-grain",
                row.package
            );
        }
    }

    /// An effect still needs its per-component grant. Deleting the shape test
    /// must not widen anyone's grant — this is the test that says so.
    #[test]
    fn an_effect_without_a_grant_is_refused_but_an_ambient_is_not() {
        let effect = ComponentImports::new([STATEMENTS_INTERFACE.to_string()]);
        let error = analyze_project(&effect, &BTreeSet::new(), "project-node")
            .expect_err("an ungranted effect must refuse");
        assert_eq!(error.error_type(), ProjectImportErrorType::UnadmittedImport);

        let granted = BTreeSet::from(["wamn:postgres".to_string()]);
        assert!(analyze_project(&effect, &granted, "project-node").is_ok());

        let ambient = ComponentImports::new(["wamn:node/types@0.2.0".to_string()]);
        assert!(
            analyze_project(&ambient, &BTreeSet::new(), "project-node").is_ok(),
            "an ambient row needs no grant"
        );
    }

    #[test]
    fn project_platform_registry_rejects_wildcards_and_interface_names() {
        for entry in ["wamn:*", "wamn:postgres/client", "wamn:postgres@0.1.0"] {
            let admitted = BTreeSet::from([entry.to_string()]);
            let error = analyze_project(
                &ComponentImports::new(Vec::new()),
                &admitted,
                "project-node",
            )
            .expect_err("the platform registry must contain exact package names");
            assert_eq!(
                error.error_type(),
                ProjectImportErrorType::InvalidPlatformImport
            );
        }
    }

    #[test]
    fn a_labelled_import_is_admitted_by_its_interface_instance_type() {
        const STATEMENTS: &str = STATEMENTS_INTERFACE;
        let granted = BTreeSet::from(["wamn:postgres".to_string()]);
        let labelled = ComponentImports::new(
            [("sql-0", Some(STATEMENTS)), ("sql-1", Some(STATEMENTS))]
                .map(|(name, implements)| instance_type(name, implements).to_string()),
        );
        assert!(analyze_project(&labelled, &granted, "project-node").is_ok());
        let error = analyze_project(&labelled, &BTreeSet::new(), "project-node")
            .expect_err("a labelled effect still needs its package grant");
        assert_eq!(error.imports(), [STATEMENTS, STATEMENTS]);

        let unlabelled = ComponentImports::new([instance_type("sql-0", None).to_string()]);
        let error = analyze_project(&unlabelled, &granted, "project-node")
            .expect_err("a label without an interface identifies no platform import");
        assert_eq!(error.imports(), ["sql-0"]);
    }

    /// Applications may import dynamic queries with the Postgres grant.
    #[test]
    fn an_application_component_may_import_client_with_a_postgres_grant() {
        let granted = BTreeSet::from(["wamn:postgres".to_string()]);
        for client in [
            instance_type("wamn:postgres/client@0.1.0", None),
            instance_type("wamn:postgres/client@0.4.0", None),
            instance_type("sql-0", Some("wamn:postgres/client@0.4.0")),
        ] {
            let imports =
                ComponentImports::new([STATEMENTS_INTERFACE.to_string(), client.to_string()]);
            assert!(analyze_project(&imports, &granted, "application").is_ok());
            assert!(analyze_project(&imports, &BTreeSet::new(), "application").is_err());
        }
        let statements = ComponentImports::new([STATEMENTS_INTERFACE.to_string()]);
        assert!(analyze_project(&statements, &granted, "application").is_ok());
    }
}

//! A release manifest loaded once for the process.
//!
//! A pod carries exactly one release, delivered by one of two carriers: the
//! digest-addressed OCI release artifact its pod template names, pulled by
//! `ReleaseManifestSource`,
//! or an immutable digest-named ConfigMap projected at
//! [`RELEASE_MANIFEST_MOUNT_PATH`]. Either way the bytes are the *sole* carrier
//! of release identity. Loading derives the manifest digest from the verified
//! content. The process keeps the parsed document for its lifetime.
//!
//! The manifest names no tenant or environment (R1): one release serves every
//! environment it is published for. The host takes its [`ReleaseScope`] from
//! its own configuration and the loaded release carries it beside the bytes.
//!
//! # Process lifetime
//!
//! Do not add invalidation, a TTL, a refresh, an eviction policy, or a
//! revalidation hook, and do not rename this a cache. The digest *is* the
//! identity: different content is a different digest, a different ConfigMap, a
//! different pod template, and therefore a different pod. There is no state in
//! which the held manifest is stale with respect to the content it was derived
//! from, so there is nothing to invalidate. `docs/deployment-simplification-
//! spec.md`'s "cache forever" means exactly process-lifetime immutability, never
//! durability.
//!
//! # Identity is derived, never asserted — and pod-to-release binding is the
//! # pod template's, by ratified design
//!
//! There is deliberately no second carrier to check the manifest against, and no
//! comparison of the bytes to the ConfigMap's own name. Such a check would not be
//! a verification: the name inside the container is a string placed by the *same
//! pod template* that mounts the bytes, so comparing them tests the template's
//! internal consistency against itself. A pod cannot second-guess its own birth
//! certificate.
//!
//! The pod template carries the OCI digest, and the registry supplies the bytes.
//! `ReleaseManifestSource`
//! refuses a digest mismatch before loading. The loaded release derives identity
//! from content alone. It asserts nothing about the source of that content.
//!
//! What binds this pod to this release is the pod template, and that is not a gap
//! — it is where the wasmCloud-v2 model assigns the fact. The template is
//! GitOps-converged, revision-controlled and rollout-controlled; binding
//! bytes-to-intent is Git's job, and Git already does it. In-container
//! name-matching would rebuild, inside the pod, an attestation the deployment
//! plane already owns.
//!
//! What *is* load-bearing survives, and it is stronger than a name check: the
//! canonical round-trip confirms the mounted bytes are well-formed manifest content
//! whose identity is computable. Corruption, truncation, or hand-editing either
//! fails the parse or shifts the digest — and a shifted digest is not a
//! masquerade, it is a correct name for different content, carried honestly into
//! every run record and authority decision downstream. Malformed bytes mean the
//! pod never goes Ready, which is the only refusal that means anything here.
//!
//! # The three enumerated readers
//!
//! Every reader consults this one instance by reference and none of them loads,
//! parses, or digest-verifies a manifest of its own:
//!
//! 1. execution — the production router driver resolves the manifest's exact
//!    component digests and wiring identity-version pairs.
//! 2. flow-http routing — serves `RouteDefinition` from
//!    [`ServingManifest::attachments`].
//! 3. jetstream delivery — gates delivery on
//!    [`ServingManifest::registrations`].
//!
//! Reader 1 runs in the executor process; readers 2 and 3 run in the wash host.
//! Those are separate processes and cannot share one object, so the
//! rule is one instance *per process*: construct once, hand it out by reference,
//! and never hold two.

use std::path::Path;

use anyhow::Context as _;
use wamn_catalog::{
    AdmittedComponent, ArtifactHash, CatalogIdentityError, ManifestDigest,
    RELEASE_MANIFEST_FILE_NAME, RELEASE_MANIFEST_MOUNT_PATH, ServingManifest,
};

/// Stable classification for a refused release load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseLoadErrorType {
    /// The manifest file is missing or unreadable.
    ManifestUnreadable,
    /// The manifest bytes failed parsing, validation, or canonicality.
    /// Loading preserves every refusal from [`ServingManifest::from_canonical_bytes`].
    ManifestRejected,
}

/// A fail-closed release load error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseLoadError {
    type_: ReleaseLoadErrorType,
    detail: Box<str>,
}

impl ReleaseLoadError {
    fn new(kind: ReleaseLoadErrorType, detail: impl Into<Box<str>>) -> Self {
        Self {
            type_: kind,
            detail: detail.into(),
        }
    }

    /// The stable classification of this refusal.
    pub fn error_type(&self) -> ReleaseLoadErrorType {
        self.type_
    }
}

impl std::fmt::Display for ReleaseLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.detail)
    }
}

impl std::error::Error for ReleaseLoadError {}

/// The release a pod carries, derived from its verified manifest content.
///
/// This is the manifest digest the production claim uses to verify the run's
/// admission pin (`ReleaseIdentity`). It is
/// host-injected identity, never guest-supplied — and, since it comes out of the
/// same bytes the readers resolve against, the two cannot disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedRelease {
    /// The serving manifest's digest — `runs.manifest_digest`. Derived from the
    /// manifest's own canonical bytes.
    pub manifest_digest: ManifestDigest,
}

/// The tenant and environment a host serves, from the host configuration.
///
/// The serving manifest carries neither (R1). The host derives the tenant from
/// its `(org, project, environment)` coordinate and every reader takes both
/// values from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseScope {
    /// The tenant of the project environment.
    pub tenant_id: String,
    /// The environment name.
    pub environment: String,
}

impl ReleaseScope {
    /// A scope of one tenant and environment.
    pub fn new(tenant_id: impl Into<String>, environment: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            environment: environment.into(),
        }
    }
}

/// The one loaded, verified serving manifest a pod resolves against.
#[derive(Debug)]
pub struct LoadedRelease {
    release: CarriedRelease,
    manifest: ServingManifest,
    scope: ReleaseScope,
}

impl LoadedRelease {
    /// Load and verify from the standard mount path.
    ///
    /// A pod whose manifest is absent, unreadable, unparseable or non-canonical
    /// must not serve, so every failure here is fatal to host construction.
    pub fn load(scope: ReleaseScope) -> Result<Self, ReleaseLoadError> {
        Self::load_from(Path::new(RELEASE_MANIFEST_MOUNT_PATH), scope)
    }

    /// Load and verify from an explicit mount root.
    ///
    /// Reads are blocking `std::fs`: this runs exactly once during host
    /// construction, before the pod serves anything, and never on a request
    /// path.
    pub fn load_from(manifest_root: &Path, scope: ReleaseScope) -> Result<Self, ReleaseLoadError> {
        // ConfigMap projections are byte-exact, and `from_canonical_bytes` admits
        // only the canonical encoding — so these bytes are used as read, with no
        // trimming. A trailing newline is a different document.
        let manifest_path = manifest_root.join(RELEASE_MANIFEST_FILE_NAME);
        let bytes = std::fs::read(&manifest_path).map_err(|error| {
            ReleaseLoadError::new(
                ReleaseLoadErrorType::ManifestUnreadable,
                format!("read serving manifest {}: {error}", manifest_path.display()),
            )
        })?;
        Self::load_canonical_bytes(&bytes, &manifest_path.display().to_string(), scope)
    }

    /// Load and verify bytes a carrier has already delivered whole.
    ///
    /// `origin` names that carrier in refusals — a mount path, or the OCI
    /// reference a
    /// `ReleaseManifestSource`
    /// checked the bytes against. It takes no part in verification: identity
    /// still comes only out of the bytes.
    pub fn load_canonical_bytes(
        bytes: &[u8],
        origin: &str,
        scope: ReleaseScope,
    ) -> Result<Self, ReleaseLoadError> {
        let (manifest, manifest_digest) =
            ServingManifest::from_canonical_bytes(bytes).map_err(|error| {
                ReleaseLoadError::new(
                    ReleaseLoadErrorType::ManifestRejected,
                    format!("serving manifest {origin} refused: {error}"),
                )
            })?;

        Ok(Self {
            release: CarriedRelease { manifest_digest },
            manifest,
            scope,
        })
    }

    /// The control serving root (docs/plan/platform-ui.md §4.2). It is built
    /// with the platform and serves only the control host routes: it has no
    /// package, component, route, attachment or database. Its fixed scope
    /// names no real tenant or environment, and a control host reads neither.
    pub fn control_root() -> Self {
        let manifest = ServingManifest {
            format_version: wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
            release: wamn_catalog::ServingRelease {
                packages: std::collections::BTreeSet::default(),
            },
            components: std::collections::BTreeSet::default(),
            routes: std::collections::BTreeSet::default(),
            attachments: std::collections::BTreeMap::default(),
            workflow: wamn_catalog::WorkflowSection::default(),
            host_routes: [wamn_catalog::HostRouteSet::Control].into(),
        };
        Self {
            release: CarriedRelease {
                manifest_digest: manifest.digest(),
            },
            manifest,
            scope: ReleaseScope::new(wamn_catalog::host_route_package(), "control"),
        }
    }

    /// The release this pod carries.
    pub fn release(&self) -> &CarriedRelease {
        &self.release
    }

    /// The verified manifest. Every reader takes it from here.
    pub fn manifest(&self) -> &ServingManifest {
        &self.manifest
    }

    /// The tenant and environment this host serves, from its configuration.
    pub fn scope(&self) -> &ReleaseScope {
        &self.scope
    }
}

/// Refuse an admitted component fact that the loaded release does not carry.
///
/// The cloud host reads the facts from the catalog and the edge reads them
/// from its release bundle. Both run this one check before loading a component.
pub fn validate_component_in_release(
    release: &LoadedRelease,
    component: &AdmittedComponent,
) -> anyhow::Result<()> {
    let manifest = release.manifest();
    let package_version = manifest
        .release
        .packages
        .iter()
        .find(|package| package.package_id() == component.scope.package_id)
        .map(wamn_catalog::PackageCoordinate::package_version);
    anyhow::ensure!(
        component.scope.tenant_id == release.scope().tenant_id
            && package_version == Some(component.scope.package_version.as_str()),
        "release-component-scope-mismatch"
    );
    let digest = ArtifactHash::parse(component.component_digest.clone())
        .context("component fact carries a non-canonical artifact hash")?;
    // Publish folds each export's call graph into the root operation, so the
    // release carries a superset of the admitted fact. The runtime trusts it.
    let carried = manifest.components.iter().any(|served| {
        served.package_id == component.scope.package_id
            && served.component == component.component
            && served.interface_version == component.interface_version
            && served.digest == digest
            && served.operations.len() == component.operations.len()
            && component.operations.iter().all(|(name, operation)| {
                served
                    .operations
                    .get(name)
                    .is_some_and(|served| served.carries(operation))
            })
    });
    anyhow::ensure!(carried, "component-not-in-carried-release");
    Ok(())
}

/// The `wamn.release` label value of a release: its 32 digest bytes as RFC 4648
/// base32, unpadded and lowercase, 52 characters.
///
/// A Kubernetes label value holds at most 63 characters, so the hex digest does
/// not fit. Base32 carries the whole digest exactly. The host puts this value in
/// its heartbeat, the operator copies it onto the `Host` resource, and the
/// `release` chart's `hostSelector` matches it (docs/plan/platform-deploy.md §9.2).
pub fn release_label(digest: &str) -> Result<String, CatalogIdentityError> {
    let digest = ManifestDigest::parse(digest)?;
    let hex = digest
        .as_str()
        .strip_prefix("sha256:")
        .expect("a parsed manifest digest has the sha256 prefix");
    let bytes = hex::decode(hex).expect("a parsed manifest digest is 64 hex characters");
    Ok(data_encoding::BASE32_NOPAD
        .encode(&bytes)
        .to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use wamn_catalog::{
        DefinitionHash, PackageCoordinate, ServingComponent, ServingComponentOperation,
        ServingRelease, ServingWiring, UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL,
    };

    use super::*;

    const COMPONENT: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const GRAPH: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn fixture() -> ServingManifest {
        ServingManifest::new(
            ServingRelease {
                packages: BTreeSet::from([PackageCoordinate::new("cat", "1.0.0").unwrap()]),
            },
            BTreeSet::from([ServingComponent {
                package_id: "cat".into(),
                component: "transform".into(),
                interface_version: "0.1".into(),
                digest: ArtifactHash::parse(COMPONENT).expect("fixture artifact hash is canonical"),
                operations: BTreeMap::from([(
                    "map".into(),
                    ServingComponentOperation {
                        pre_commit: None,
                        committed_result_schema: None,
                        fresh_only: false,
                        registered_operation: None,
                        permissions: BTreeSet::new(),
                        participant: None,
                        statements: BTreeMap::new(),
                    },
                )]),
            }]),
            BTreeSet::new(),
            BTreeSet::from([ServingWiring {
                package_id: "cat".into(),
                wiring_id: "orders".into(),
                wiring_version: 2,
                graph_hash: DefinitionHash::parse(GRAPH)
                    .expect("fixture definition hash is canonical"),
            }]),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .expect("fixture manifest is valid")
    }

    fn scope() -> ReleaseScope {
        ReleaseScope::new("t1", "prod")
    }

    /// A private scratch mount, named for its test so runs cannot collide.
    struct Mounts {
        root: PathBuf,
    }

    impl Mounts {
        fn new(test: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("wamn-release-load-{}-{test}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("manifest")).expect("scratch manifest dir");
            Self { root }
        }

        fn manifest_dir(&self) -> PathBuf {
            self.root.join("manifest")
        }

        fn write_manifest_bytes(&self, bytes: &[u8]) -> &Self {
            std::fs::write(self.manifest_dir().join(RELEASE_MANIFEST_FILE_NAME), bytes)
                .expect("write manifest");
            self
        }

        fn load(&self) -> Result<LoadedRelease, ReleaseLoadError> {
            LoadedRelease::load_from(&self.manifest_dir(), scope())
        }
    }

    impl Drop for Mounts {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn release_label_is_the_digest_as_lowercase_unpadded_base32() {
        let label = release_label(
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        )
        .expect("a canonical digest");
        assert_eq!(
            label,
            "4oymiquy7qobjgx36tejs35zeqt24qpemsnzgtfeswmrw6csxbkq"
        );
        assert_eq!(label.len(), 52);
        let zero = format!("sha256:{}", "0".repeat(64));
        assert_eq!(release_label(&zero).expect("digest"), "a".repeat(52));
        for refused in [
            "e3b0",
            "sha256:E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855",
            "sha256:00",
        ] {
            assert!(release_label(refused).is_err(), "{refused} is refused");
        }
    }

    #[test]
    fn a_well_formed_mount_loads_and_verifies_once() {
        let mounts = Mounts::new("ok");
        let expected = fixture();
        mounts.write_manifest_bytes(&expected.canonical_bytes());

        let loaded_release = mounts.load().expect("well-formed mount loads");

        assert_eq!(loaded_release.manifest(), &expected);
        assert_eq!(
            loaded_release.manifest().components,
            expected.components,
            "the loaded release retains the exact component closure"
        );
        assert_eq!(
            loaded_release.manifest().workflow,
            expected.workflow,
            "the loaded release retains the exact workflow section"
        );
    }

    #[test]
    fn the_release_identity_comes_from_the_verified_content() {
        let mounts = Mounts::new("derived-identity");
        let expected = fixture();
        mounts.write_manifest_bytes(&expected.canonical_bytes());

        let loaded_release = mounts.load().expect("well-formed mount loads");

        // No carrier asserts the identity: the digest is over the same bytes the
        // pod resolves against, so the two are, by construction, the same fact.
        assert_eq!(loaded_release.release().manifest_digest, expected.digest());
    }

    #[test]
    fn an_absent_manifest_refuses() {
        let mounts = Mounts::new("no-manifest");

        assert_eq!(
            mounts
                .load()
                .expect_err("absent manifest refuses")
                .error_type(),
            ReleaseLoadErrorType::ManifestUnreadable
        );
    }

    #[test]
    fn bytes_that_are_not_a_manifest_refuse() {
        let mounts = Mounts::new("garbage");
        mounts.write_manifest_bytes(b"{ not a manifest");

        assert_eq!(
            mounts.load().expect_err("garbage refuses").error_type(),
            ReleaseLoadErrorType::ManifestRejected
        );
    }

    #[test]
    fn an_unsupported_format_refuses_with_the_frozen_literal() {
        let mounts = Mounts::new("unsupported-format");
        mounts.write_manifest_bytes(
            br#"{"attachments":{},"components":[],"format-version":0,"registrations":{},"release":{"packages":[{"package-id":"cat","package-version":"1.0.0"}]},"wirings":[]}"#,
        );

        let error = mounts
            .load()
            .expect_err("unsupported format refuses at the loaded release");
        assert_eq!(error.error_type(), ReleaseLoadErrorType::ManifestRejected);
        assert!(
            error
                .to_string()
                .contains(UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL),
            "the loaded release must preserve the typed format refusal: {error}"
        );
    }

    #[test]
    fn a_trailing_newline_on_the_manifest_refuses() {
        let mounts = Mounts::new("manifest-newline");
        let mut bytes = fixture().canonical_bytes();
        bytes.push(b'\n');
        mounts.write_manifest_bytes(&bytes);

        // The manifest's encoding IS its identity, so these bytes are never
        // trimmed and never re-canonicalized: a repaired document would derive a
        // digest naming content nobody shipped.
        assert_eq!(
            mounts
                .load()
                .expect_err("trailing newline refuses")
                .error_type(),
            ReleaseLoadErrorType::ManifestRejected
        );
    }

    #[test]
    fn a_re_indented_manifest_refuses() {
        let mounts = Mounts::new("manifest-reindented");
        let value: serde_json::Value =
            serde_json::from_slice(&fixture().canonical_bytes()).expect("canonical bytes parse");
        let pretty = serde_json::to_vec_pretty(&value).expect("re-indent");
        mounts.write_manifest_bytes(&pretty);

        // Same content, different encoding: refused rather than served under a
        // digest it would not derive.
        assert_eq!(
            mounts
                .load()
                .expect_err("non-canonical refuses")
                .error_type(),
            ReleaseLoadErrorType::ManifestRejected
        );
    }

    fn carried_fact() -> AdmittedComponent {
        AdmittedComponent {
            scope: wamn_catalog::ComponentPackageScope {
                tenant_id: "t1".into(),
                package_id: "cat".into(),
                package_version: "1.0.0".into(),
            },
            component: "transform".into(),
            interface_version: "0.1".into(),
            operations: BTreeMap::from([(
                "map".into(),
                wamn_catalog::AdmittedComponentOperation {
                    pre_commit: None,
                    pre_commit_required: false,
                    registered_operation: None,
                    fresh_only: false,
                    committed_result_schema: None,
                    dependencies: Vec::new(),
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    statements: BTreeMap::new(),
                },
            )]),
            component_digest: COMPONENT.into(),
            imports: Vec::new(),
            imports_fingerprint: String::new(),
            effects: Vec::new(),
        }
    }

    /// One change to a carried fact.
    type Mutation = fn(&mut AdmittedComponent);

    #[test]
    fn an_admitted_component_the_release_does_not_carry_is_refused() {
        let release =
            LoadedRelease::load_canonical_bytes(&fixture().canonical_bytes(), "fixture", scope())
                .expect("the fixture loads");
        validate_component_in_release(&release, &carried_fact()).expect("the carried fact passes");

        let mutations: [(&str, Mutation); 5] = [
            ("another tenant", |fact| fact.scope.tenant_id = "t2".into()),
            ("another package version", |fact| {
                fact.scope.package_version = "2.0.0".into();
            }),
            ("another digest", |fact| {
                fact.component_digest =
                    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                        .into();
            }),
            ("another component", |fact| fact.component = "other".into()),
            ("another operation", |fact| {
                let operation = fact.operations["map"].clone();
                fact.operations.insert("reduce".into(), operation);
            }),
        ];
        for (change, mutate) in mutations {
            let mut fact = carried_fact();
            mutate(&mut fact);
            assert!(
                validate_component_in_release(&release, &fact).is_err(),
                "a fact with {change} is refused"
            );
        }
    }
}

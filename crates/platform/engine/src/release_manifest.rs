//! An immutable verified release for one project deployment and digest.
//!
//! The manifest bytes determine release identity. The deployment supplies its
//! trusted project, environment and application membership. Readers share this
//! loaded instance while the release has active invocations or unfinished runs.
//! Identical component bytes do not share deployment authority or credentials.

use std::{path::Path, sync::Arc};

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
    fn new(type_: ReleaseLoadErrorType, detail: impl Into<Box<str>>) -> Self {
        Self {
            type_,
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
/// This is the manifest digest the production lease uses to verify the run's
/// admission pin (`ReleaseIdentity`). It is
/// host-injected identity, never guest-supplied — and, since it comes out of the
/// same bytes the readers resolve against, the two cannot disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedRelease {
    /// The serving manifest's digest — `runs.manifest_digest`. Derived from the
    /// manifest's own canonical bytes.
    pub manifest_digest: ManifestDigest,
}

/// The project, environment and application a host serves, from the
/// host configuration.
///
/// The serving manifest carries none of them (R1). The host derives the
/// project from its `(org, project, environment)` coordinate, takes the
/// targeted application that `environment.k` declares, and every reader takes it
/// from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTarget {
    /// The composed `org--env--project` key of the project environment.
    pub project_env_key: String,
    /// The short environment name, such as `dev`.
    pub environment: String,
    /// The single application name and its packages
    /// (docs/architecture/execution.md#workflows).
    pub applications: wamn_catalog::EnvironmentApplications,
}

impl ReleaseTarget {
    /// A target of one project and environment that declares no application.
    pub fn new(project_env_key: impl Into<String>, environment: impl Into<String>) -> Self {
        Self {
            project_env_key: project_env_key.into(),
            environment: environment.into(),
            applications: wamn_catalog::EnvironmentApplications::new(),
        }
    }

    /// This target with the targeted application membership.
    #[must_use]
    pub fn with_applications(
        mut self,
        applications: wamn_catalog::EnvironmentApplications,
    ) -> Self {
        self.applications = applications;
        self
    }
}

/// A verified serving manifest bound to one project deployment.
#[derive(Debug)]
pub struct LoadedRelease {
    release: CarriedRelease,
    manifest: ServingManifest,
    target: ReleaseTarget,
    components: Arc<[AdmittedComponent]>,
}

impl LoadedRelease {
    /// Load and verify from the standard mount path.
    ///
    /// A pod whose manifest is absent, unreadable, unparseable or non-canonical
    /// must not serve that release. Other releases can continue.
    pub fn load(target: ReleaseTarget) -> Result<Self, ReleaseLoadError> {
        Self::load_from(Path::new(RELEASE_MANIFEST_MOUNT_PATH), target)
    }

    /// Load and verify from an explicit mount root.
    ///
    /// Reads are blocking `std::fs`: this runs exactly once during host
    /// construction, before the pod serves anything, and never on a request
    /// path.
    pub fn load_from(
        manifest_root: &Path,
        target: ReleaseTarget,
    ) -> Result<Self, ReleaseLoadError> {
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
        Self::load_canonical_bytes(&bytes, &manifest_path.display().to_string(), target)
    }

    /// Load and verify bytes a carrier has already delivered whole.
    ///
    /// `origin` names that carrier in refusals — a mount path, or the OCI
    /// reference a
    /// `ReleaseManifestSource`
    /// validated the bytes against. It takes no part in verification: identity
    /// still comes only out of the bytes.
    pub fn load_canonical_bytes(
        bytes: &[u8],
        origin: &str,
        target: ReleaseTarget,
    ) -> Result<Self, ReleaseLoadError> {
        let (manifest, manifest_digest) =
            ServingManifest::from_canonical_bytes(bytes).map_err(|error| {
                ReleaseLoadError::new(
                    ReleaseLoadErrorType::ManifestRejected,
                    format!("serving manifest {origin} refused: {error}"),
                )
            })?;

        if target.applications.len() > 1 {
            return Err(ReleaseLoadError::new(
                ReleaseLoadErrorType::ManifestRejected,
                "a loaded release serves at most one application",
            ));
        }
        if let Some((application, membership)) = target.applications.first_key_value()
            && manifest
                .release
                .packages
                .iter()
                .any(|package| !membership.packages.contains(package.package_id()))
        {
            return Err(ReleaseLoadError::new(
                ReleaseLoadErrorType::ManifestRejected,
                format!("release carries a package outside application {application}"),
            ));
        }
        let components = manifest
            .admitted_components(&target.project_env_key)
            .map_err(|error| {
                ReleaseLoadError::new(ReleaseLoadErrorType::ManifestRejected, error.to_string())
            })?;
        for component in &components {
            wamn_catalog::verify_stored_effect_projection(component).map_err(|error| {
                ReleaseLoadError::new(ReleaseLoadErrorType::ManifestRejected, error.to_string())
            })?;
        }
        Ok(Self {
            release: CarriedRelease { manifest_digest },
            manifest,
            target,
            components: components.into(),
        })
    }

    /// The control serving root
    /// (docs/architecture/execution.md#host-run-routes). It is built with the
    /// platform and serves only the control host routes: it has no package,
    /// component, route, attachment or database. Its fixed target names no real
    /// project or environment, and a control host reads neither.
    pub fn control_root() -> Self {
        let manifest = ServingManifest {
            composed_entry: None,
            format_version: wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
            release: wamn_catalog::ServingRelease {
                packages: std::collections::BTreeSet::default(),
            },
            components: std::collections::BTreeSet::default(),
            routes: std::collections::BTreeSet::default(),
            attachments: std::collections::BTreeMap::default(),

            host_routes: [wamn_catalog::HostRouteSet::Control].into(),
        };
        Self {
            release: CarriedRelease {
                manifest_digest: manifest.digest(),
            },
            manifest,
            target: ReleaseTarget::new(wamn_catalog::host_route_package(), "control"),
            components: Vec::new().into(),
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

    /// Admitted facts validated once when this release loads.
    pub fn components(&self) -> &Arc<[AdmittedComponent]> {
        &self.components
    }

    /// The project and environment this host serves, from its configuration.
    pub fn target(&self) -> &ReleaseTarget {
        &self.target
    }
}

/// Refuse an admitted component fact that the loaded release does not carry.
///
/// The cloud host reads the facts from the catalog and the edge reads them
/// from its release bundle. Both run this one test before loading a component.
pub fn validate_component_in_release(
    release: &LoadedRelease,
    component: &AdmittedComponent,
) -> anyhow::Result<()> {
    let manifest = release.manifest();
    let package_version = manifest
        .release
        .packages
        .iter()
        .find(|package| package.package_id() == component.target.package_id)
        .map(wamn_catalog::PackageCoordinate::package_version)
        .or_else(|| {
            (component.target.package_id == wamn_catalog::PALETTE_PACKAGE_ID)
                .then_some(wamn_catalog::PALETTE_PACKAGE_VERSION)
        });
    anyhow::ensure!(
        component.target.project_env_key == release.target().project_env_key
            && package_version == Some(component.target.package_version.as_str()),
        "release-component-target-mismatch"
    );
    let digest = ArtifactHash::parse(component.component_digest.clone())
        .context("component fact carries a non-canonical artifact hash")?;
    // The descriptor carries the admitted operations without another projection.
    let carried = manifest.components.iter().any(|served| {
        served.package_id == component.target.package_id
            && served.descriptor.component == component.component
            && served.descriptor.interface_version == component.interface_version
            && served.descriptor.component_digest == digest.as_str()
            && served.descriptor.operations == component.operations
    });
    anyhow::ensure!(carried, "component-not-in-carried-release");
    Ok(())
}

/// The `wamn.release` label value of a release: its 32 digest bytes as RFC 4648
/// base32, unpadded and lowercase, 52 characters.
///
/// A Kubernetes label value holds at most 63 characters, so the hex digest does
/// not fit. Base32 carries the whole digest exactly. The host puts this value
/// in its heartbeat, the operator copies it onto the `Host` resource, and the
/// `release` chart's `hostSelector` matches it
/// (docs/architecture/overview.md#environment-lifecycle).
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
        AdmittedComponentOperation, PackageCoordinate, ServingComponent, ServingRelease,
        UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL,
    };

    use super::*;

    const COMPONENT: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn fixture() -> ServingManifest {
        ServingManifest::new(
            ServingRelease {
                packages: BTreeSet::from([PackageCoordinate::new("cat", "1.0.0").unwrap()]),
            },
            BTreeSet::from([ServingComponent {
                runtime_schema: None,
                statement_instances: std::collections::BTreeMap::new(),
                package_id: "cat".into(),
                descriptor: {
                    let mut descriptor =
                        wamn_catalog::ComponentDescriptor::named("transform", "0.1", COMPONENT);
                    descriptor.operations = BTreeMap::from([(
                        "map".into(),
                        AdmittedComponentOperation {
                            connections: Vec::new(),
                            host_transaction: false,
                            input_ports: Vec::new(),
                            output_ports: Vec::new(),
                            parameters: Vec::new(),
                            committed_result_schema: None,
                            registered_operation: None,
                            statements: BTreeMap::new(),
                        },
                    )]);
                    descriptor
                },
            }]),
            BTreeSet::new(),
            BTreeMap::new(),
        )
        .expect("fixture manifest is valid")
    }

    #[test]
    fn a_loaded_release_refuses_multiple_applications_and_foreign_packages() {
        let bytes = fixture().canonical_bytes();
        let mut targeted = target();
        targeted
            .applications
            .insert("other".into(), targeted.applications["catalog"].clone());
        assert_eq!(
            LoadedRelease::load_canonical_bytes(&bytes, "fixture", targeted)
                .unwrap_err()
                .error_type(),
            ReleaseLoadErrorType::ManifestRejected
        );
        let mut targeted = target();
        targeted
            .applications
            .get_mut("catalog")
            .unwrap()
            .packages
            .clear();
        assert_eq!(
            LoadedRelease::load_canonical_bytes(&bytes, "fixture", targeted)
                .unwrap_err()
                .error_type(),
            ReleaseLoadErrorType::ManifestRejected
        );
    }

    #[test]
    fn loading_refuses_an_effect_projection_its_imports_do_not_derive() {
        let mut manifest = fixture();
        let mut component = manifest.components.pop_first().unwrap();
        component.descriptor.imports = vec!["wamn:postgres/client@0.1.0".to_owned()];
        manifest.components.insert(component);
        let error = LoadedRelease::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "effect projection",
            target(),
        )
        .unwrap_err();
        assert_eq!(error.error_type(), ReleaseLoadErrorType::ManifestRejected);
    }

    #[test]
    fn loading_admits_an_effect_projection_its_imports_derive() {
        let mut manifest = fixture();
        let mut component = manifest.components.pop_first().unwrap();
        component.descriptor.imports = vec!["wasi:clocks/monotonic-clock@0.2.3".to_owned()];
        manifest.components.insert(component);
        LoadedRelease::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "effect projection",
            target(),
        )
        .unwrap();
    }

    fn target() -> ReleaseTarget {
        ReleaseTarget::new("t1", "prod").with_applications(
            [(
                "catalog".to_owned(),
                wamn_catalog::ApplicationDefinition {
                    connections: std::collections::BTreeMap::new(),
                    packages: ["cat".to_owned()].into(),
                },
            )]
            .into(),
        )
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
            LoadedRelease::load_from(&self.manifest_dir(), target())
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
            target: wamn_catalog::ComponentPackageCoordinate {
                project_env_key: "t1".into(),
                package_id: "cat".into(),
                package_version: "1.0.0".into(),
            },
            component: "transform".into(),
            interface_version: "0.1".into(),
            operations: BTreeMap::from([(
                "map".into(),
                wamn_catalog::AdmittedComponentOperation {
                    connections: Vec::new(),
                    registered_operation: None,
                    committed_result_schema: None,
                    host_transaction: false,
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
            LoadedRelease::load_canonical_bytes(&fixture().canonical_bytes(), "fixture", target())
                .expect("the fixture loads");
        validate_component_in_release(&release, &carried_fact()).expect("the carried fact passes");

        let mutations: [(&str, Mutation); 5] = [
            ("another project", |fact| {
                fact.target.project_env_key = "t2".into();
            }),
            ("another package version", |fact| {
                fact.target.package_version = "2.0.0".into();
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

//! The edge's verified serving manifest loader.
use crate::serving_context::{ReleaseTarget, ServingContext};
use std::path::Path;
use wamn_component_policy::AdmittedComponent;
use wamn_edge_document::{RELEASE_MANIFEST_FILE_NAME, ServingManifest};

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

/// Load and verify from an explicit mount root.
///
/// Reads are blocking `std::fs`: this runs exactly once during host
/// construction, before the pod serves anything, and never on a request
/// path.
pub fn load_from(
    manifest_root: &Path,
    target: ReleaseTarget,
) -> Result<ServingContext, ReleaseLoadError> {
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
    load_canonical_bytes(&bytes, &manifest_path.display().to_string(), target)
}

/// Verify bytes delivered by an edge release bundle.
pub fn load_canonical_bytes(
    bytes: &[u8],
    origin: &str,
    target: ReleaseTarget,
) -> Result<ServingContext, ReleaseLoadError> {
    let (manifest, _) = ServingManifest::from_canonical_bytes(bytes).map_err(|error| {
        ReleaseLoadError::new(
            ReleaseLoadErrorType::ManifestRejected,
            format!("serving manifest {origin} refused: {error}"),
        )
    })?;
    ServingContext::new(manifest, target).map_err(|error| {
        ReleaseLoadError::new(ReleaseLoadErrorType::ManifestRejected, error.to_string())
    })
}

/// Refuse an admitted component fact that the loaded release does not carry.
///
/// The edge reads these facts from its release bundle before loading a component.
pub fn validate_component_in_release(
    release: &ServingContext,
    component: &AdmittedComponent,
) -> anyhow::Result<()> {
    let manifest = release.manifest();
    let version_matches = manifest
        .release
        .packages
        .iter()
        .find(|package| package.package_id() == &component.target.package_id)
        .map_or_else(
            || {
                component.target.package_id.as_str() == wamn_package::PALETTE_PACKAGE_ID
                    && component.target.package_version.as_str()
                        == wamn_package::PALETTE_PACKAGE_VERSION
            },
            |package| package.package_version() == &component.target.package_version,
        );
    anyhow::ensure!(
        component.target.project_env_key == release.target().project_env_key && version_matches,
        "release-component-target-mismatch"
    );
    let digest = component.component_digest;
    // The descriptor carries the admitted operations without another projection.
    let carried = manifest.components.iter().any(|served| {
        served.package_id == component.target.package_id
            && served.descriptor.component == component.component
            && served.descriptor.interface_version == component.interface_version
            && served.descriptor.component_digest == digest
            && served.descriptor.operations == component.operations
            && served.descriptor.data_change == component.data_change
    });
    anyhow::ensure!(carried, "component-not-in-carried-release");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use wamn_component_policy::{AdmittedComponentOperation, ComponentPackageCoordinate};
    use wamn_edge_document::{
        SERVING_MANIFEST_FORMAT_VERSION, ServingComponent, ServingRelease,
        UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL,
    };
    use wamn_package::PackageCoordinate;

    use super::*;
    use super as loader;

    const COMPONENT: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn fixture() -> ServingManifest {
        ServingManifest {
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release: ServingRelease {
                packages: BTreeSet::from([PackageCoordinate::new(
                    "cat".parse().unwrap(),
                    "1.0.0".parse().unwrap(),
                )
                .unwrap()]),
            },
            components: BTreeSet::from([ServingComponent::project(
                &AdmittedComponent {
                    data_change: false,
                    target: ComponentPackageCoordinate {
                        project_env_key: "org-a--dev--project".parse().unwrap(),
                        package_id: "cat".parse().unwrap(),
                        package_version: "1.0.0".parse().unwrap(),
                    },
                    component: "transform".parse().unwrap(),
                    interface_version: "0.1".parse().unwrap(),
                    operations: BTreeMap::from([(
                        "map".parse().unwrap(),
                        AdmittedComponentOperation {
                            host_transaction: false,
                            input_ports: Vec::new(),
                            output_ports: Vec::new(),
                            parameters: Vec::new(),
                            committed_result_schema: None,
                            registered_operation: None,
                        },
                    )]),
                    component_digest: COMPONENT.parse().unwrap(),
                    imports: Vec::new(),
                    imports_fingerprint: wamn_digest::canonical_json_sha256(&serde_json::json!([])),
                    effects: Vec::new(),
                },
                Vec::new(),
            )]),
            composed_entry: None,
            routes: BTreeSet::new(),
            attachments: BTreeMap::new(),
            host_routes: BTreeSet::new(),
        }
    }

    #[test]
    fn a_loaded_release_refuses_multiple_applications_and_foreign_packages() {
        let bytes = fixture().canonical_bytes();
        let catalog = |packages: BTreeSet<wamn_package::PackageName>| {
            wamn_registry::ApplicationDefinition {
                connections: BTreeMap::new(),
                packages,
            }
        };
        let mut targeted = target();
        targeted.applications = BTreeMap::from([
            ("catalog".parse().unwrap(), catalog(["cat".parse().unwrap()].into())),
            ("other".parse().unwrap(), catalog(["cat".parse().unwrap()].into())),
        ]);
        assert_eq!(
            loader::load_canonical_bytes(&bytes, "fixture", targeted)
                .unwrap_err()
                .error_type(),
            ReleaseLoadErrorType::ManifestRejected
        );
        let mut targeted = target();
        targeted.applications =
            BTreeMap::from([("catalog".parse().unwrap(), catalog(BTreeSet::new()))]);
        assert_eq!(
            loader::load_canonical_bytes(&bytes, "fixture", targeted)
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
        let error = loader::load_canonical_bytes(
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
        loader::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "effect projection",
            target(),
        )
        .unwrap();
    }

    fn target() -> ReleaseTarget {
        ReleaseTarget::new("t1--dev--project".parse().unwrap(), "prod".parse().unwrap())
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

        fn load(&self) -> Result<ServingContext, ReleaseLoadError> {
            loader::load_from(&self.manifest_dir(), target())
        }
    }

    impl Drop for Mounts {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
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
                .contains(UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL)
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
            data_change: false,
            target: wamn_component_policy::ComponentPackageCoordinate {
                project_env_key: "t1--dev--project".parse().unwrap(),
                package_id: "cat".parse().unwrap(),
                package_version: "1.0.0".parse().unwrap(),
            },
            component: "transform".parse().unwrap(),
            interface_version: "0.1".parse().unwrap(),
            operations: BTreeMap::from([(
                "map".parse().unwrap(),
                wamn_component_policy::AdmittedComponentOperation {
                    registered_operation: None,
                    committed_result_schema: None,
                    host_transaction: false,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                },
            )]),
            component_digest: COMPONENT.parse().unwrap(),
            imports: Vec::new(),
            imports_fingerprint: wamn_digest::canonical_json_sha256(&serde_json::json!([])),
            effects: Vec::new(),
        }
    }

    /// One change to a carried fact.
    type Mutation = fn(&mut AdmittedComponent);

    #[test]
    fn an_admitted_component_the_release_does_not_carry_is_refused() {
        let release =
            loader::load_canonical_bytes(&fixture().canonical_bytes(), "fixture", target())
                .expect("the fixture loads");
        validate_component_in_release(&release, &carried_fact()).expect("the carried fact passes");

        let mutations: [(&str, Mutation); 5] = [
            ("another project", |fact| {
                fact.target.project_env_key = "t2--dev--project".parse().unwrap();
            }),
            ("another package version", |fact| {
                fact.target.package_version = "2.0.0".parse().unwrap();
            }),
            ("another digest", |fact| {
                fact.component_digest =
                    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                        .parse()
                        .unwrap();
            }),
            ("another component", |fact| {
                fact.component = "other".parse().unwrap()
            }),
            ("another operation", |fact| {
                let operation = fact.operations["map"].clone();
                fact.operations.insert("reduce".parse().unwrap(), operation);
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

//! A platform-published release loaded from a local directory.
//!
//! Publish writes the bundle, and the box never assembles one. The directory
//! holds:
//!
//! - `edge-release.json`: the bundle, which names the SHA-256 of the next
//!   four files.
//! - The canonical serving manifest.
//! - `components.json`: the admitted component facts of the release, which the
//!   cloud host reads from its catalog.
//! - `grants.json`: the permissions of each role ([`Grants`]).
//! - `flow-http.wasm`: the composed entry that serves the routes, the
//!   http-route guest composed with the application at publish. The box
//!   composes nothing.
//! - `<sha256>.wasm`: the bytes of each component.
//!
//! The edge configuration pins the bundle digest, as a pod template pins the
//! manifest digest. One pinned digest covers every file, and a later release
//! signature signs that same digest. The loader refuses any file whose bytes do
//! not match.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use wamn_catalog::edge_bundle::{
    BUNDLE_FILE_NAME, BUNDLE_FORMAT, COMPONENTS_FILE_NAME, EdgeBundle, GRANTS_FILE_NAME,
    INGRESS_FILE_NAME, file_digest,
};
use wamn_catalog::{AdmittedComponent, RELEASE_MANIFEST_FILE_NAME};
use wamn_engine::artifact_source::{ArtifactSource as _, LocalComponentSource};
use wamn_engine::operation::native_workload::NativeComponent;
use wamn_engine::release_manifest::{LoadedRelease, ReleaseScope, validate_component_in_release};

use crate::grants::Grants;

/// A release, its component facts, its grants and its composed entry, each
/// checked against the pinned bundle digest.
pub struct EdgeRelease {
    bundle_digest: String,
    release: Arc<LoadedRelease>,
    components: Arc<[AdmittedComponent]>,
    /// The verified bytes of each component, in the order of `components`.
    bodies: Vec<Vec<u8>>,
    grants: Grants,
    ingress: Vec<u8>,
}

/// Hand-written so a debug print names the guest by size, not by its bytes.
impl fmt::Debug for EdgeRelease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EdgeRelease")
            .field("bundle_digest", &self.bundle_digest)
            .field("release", &self.release)
            .field("components", &self.components)
            .field(
                "body_bytes",
                &self.bodies.iter().map(Vec::len).sum::<usize>(),
            )
            .field("grants", &self.grants)
            .field("ingress_bytes", &self.ingress.len())
            .finish()
    }
}

impl EdgeRelease {
    /// Load the bundle in `directory` whose `edge-release.json` has
    /// `bundle_digest`, and read every component body once. `scope` is the
    /// env_key and environment of the box, from its configuration.
    pub async fn load(
        directory: &Path,
        bundle_digest: &str,
        scope: ReleaseScope,
    ) -> Result<Self, EdgeReleaseError> {
        let bundle_bytes = read_pinned(directory, BUNDLE_FILE_NAME, bundle_digest)?;
        let bundle: EdgeBundle = serde_json::from_slice(&bundle_bytes)
            .map_err(|error| rejected(format!("{BUNDLE_FILE_NAME} is not a bundle: {error}")))?;
        if bundle.format != BUNDLE_FORMAT {
            return Err(rejected(format!(
                "{BUNDLE_FILE_NAME} has format {}, and this edge reads {BUNDLE_FORMAT}",
                bundle.format
            )));
        }

        let manifest_bytes = read_pinned(directory, RELEASE_MANIFEST_FILE_NAME, &bundle.manifest)?;
        let release =
            LoadedRelease::load_canonical_bytes(&manifest_bytes, RELEASE_MANIFEST_FILE_NAME, scope)
                .map_err(|error| rejected(error.to_string()))?;

        let components_bytes = read_pinned(directory, COMPONENTS_FILE_NAME, &bundle.components)?;
        let components: Vec<AdmittedComponent> = serde_json::from_slice(&components_bytes)
            .map_err(|error| {
                rejected(format!(
                    "{COMPONENTS_FILE_NAME} is not a component fact list: {error}"
                ))
            })?;
        check_one_fact_per_component(&release, &components)?;
        let source = LocalComponentSource::new(directory.to_path_buf());
        let mut bodies = Vec::with_capacity(components.len());
        for component in &components {
            bodies.push(source.pull_verified(component).await.map_err(|error| {
                EdgeReleaseError::new(EdgeReleaseErrorType::Mismatch, error.to_string())
            })?);
        }

        let grants_bytes = read_pinned(directory, GRANTS_FILE_NAME, &bundle.grants)?;
        let grants = Grants::parse(&grants_bytes, release.manifest())?;
        let ingress = read_pinned(directory, INGRESS_FILE_NAME, &bundle.ingress)?;

        Ok(Self {
            bundle_digest: bundle_digest.to_owned(),
            release: Arc::new(release),
            components: components.into(),
            bodies,
            grants,
            ingress,
        })
    }

    /// The pinned digest of `edge-release.json`.
    pub fn bundle_digest(&self) -> &str {
        &self.bundle_digest
    }

    /// The loaded serving manifest and its identity.
    pub fn release(&self) -> &Arc<LoadedRelease> {
        &self.release
    }

    /// The admitted component facts, one per manifest component.
    pub fn components(&self) -> &Arc<[AdmittedComponent]> {
        &self.components
    }

    /// Each admitted fact with its verified bytes, for the native loader.
    pub fn native_components(&self) -> Vec<NativeComponent> {
        self.components
            .iter()
            .zip(&self.bodies)
            .map(|(fact, bytes)| NativeComponent {
                fact: fact.clone(),
                bytes: bytes.clone(),
            })
            .collect()
    }

    /// The permissions of each role.
    pub fn grants(&self) -> &Grants {
        &self.grants
    }

    /// The bytes of the composed entry.
    pub fn ingress(&self) -> &[u8] {
        &self.ingress
    }
}

/// Require exactly one admitted fact for each manifest component, each one
/// carried by the release.
fn check_one_fact_per_component(
    release: &LoadedRelease,
    components: &[AdmittedComponent],
) -> Result<(), EdgeReleaseError> {
    let mut seen = BTreeSet::new();
    for component in components {
        validate_component_in_release(release, component).map_err(|error| {
            rejected(format!(
                "{COMPONENTS_FILE_NAME} fact {}/{}: {error}",
                component.scope.package_id, component.component
            ))
        })?;
        let key = (
            component.scope.package_id.as_str(),
            component.component.as_str(),
        );
        if !seen.insert(key) {
            return Err(rejected(format!(
                "{COMPONENTS_FILE_NAME} repeats {}/{}",
                key.0, key.1
            )));
        }
    }
    let manifest = release.manifest();
    if let Some(missing) = manifest.components.iter().find(|served| {
        !seen.contains(&(
            served.package_id.as_str(),
            served.descriptor.component.as_str(),
        ))
    }) {
        return Err(rejected(format!(
            "{COMPONENTS_FILE_NAME} has no fact for {}/{}",
            missing.package_id, missing.descriptor.component
        )));
    }
    Ok(())
}

/// Read `name` under `directory` and refuse bytes whose SHA-256 is not `pinned`.
fn read_pinned(directory: &Path, name: &str, pinned: &str) -> Result<Vec<u8>, EdgeReleaseError> {
    let path = directory.join(name);
    let bytes = std::fs::read(&path).map_err(|error| {
        EdgeReleaseError::new(
            EdgeReleaseErrorType::Unreadable,
            format!("read {}: {error}", path.display()),
        )
    })?;
    let digest = file_digest(&bytes);
    if digest != pinned {
        return Err(EdgeReleaseError::new(
            EdgeReleaseErrorType::Mismatch,
            format!("{name} has digest {digest}, and the release pins {pinned}"),
        ));
    }
    Ok(bytes)
}

/// Stable classification for a refused edge release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeReleaseErrorType {
    /// A bundle file is missing or unreadable.
    Unreadable,
    /// A file's bytes do not match the digest that pins them.
    Mismatch,
    /// A file matches its digest, and its content is refused.
    Rejected,
}

/// A refused edge release, with the file and the reason.
#[derive(Debug)]
pub struct EdgeReleaseError {
    type_: EdgeReleaseErrorType,
    detail: Box<str>,
}

impl EdgeReleaseError {
    pub(crate) fn new(kind: EdgeReleaseErrorType, detail: impl Into<Box<str>>) -> Self {
        Self {
            type_: kind,
            detail: detail.into(),
        }
    }

    /// Return the stable failure class.
    pub fn error_type(&self) -> EdgeReleaseErrorType {
        self.type_
    }
}

impl fmt::Display for EdgeReleaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "edge release refused: {}", self.detail)
    }
}

impl std::error::Error for EdgeReleaseError {}

fn rejected(detail: String) -> EdgeReleaseError {
    EdgeReleaseError::new(EdgeReleaseErrorType::Rejected, detail)
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;
    use std::collections::BTreeMap;
    use wamn_catalog::{
        AdmittedComponentOperation, ComponentDescriptor, ComponentPackageScope, PackageCoordinate,
        ServingComponent, ServingComponentOperation, ServingManifest, ServingRelease,
    };

    pub(crate) const OPERATION: &str = "scale:device/record@1.0.0";

    pub(crate) async fn release() -> EdgeRelease {
        let operation = ServingComponentOperation {
            registered_operation: Some(OPERATION.into()),
            permissions: BTreeSet::from([OPERATION.into()]),
            committed_result_schema: None,
            statements: BTreeMap::new(),
        };
        let fact = AdmittedComponent {
            scope: ComponentPackageScope {
                env_key: "tenant-a".into(),
                package_id: "scale".into(),
                package_version: "1.0.0".into(),
            },
            component: "device".into(),
            interface_version: "0.1".into(),
            component_digest: file_digest(b"device"),
            operations: BTreeMap::from([(
                OPERATION.into(),
                AdmittedComponentOperation {
                    registered_operation: Some(OPERATION.into()),
                    committed_result_schema: None,
                    host_transaction: false,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    statements: BTreeMap::new(),
                },
            )]),
            imports: Vec::new(),
            imports_fingerprint: String::new(),
            effects: Vec::new(),
        };
        let manifest = ServingManifest::new(
            ServingRelease {
                packages: BTreeSet::from([PackageCoordinate::new("scale", "1.0.0").unwrap()]),
            },
            BTreeSet::from([ServingComponent {
                runtime_schema: None,
                statement_instances: BTreeMap::new(),
                package_id: "scale".into(),
                descriptor: ComponentDescriptor::named("device", "0.1", &fact.component_digest),
                operations: BTreeMap::from([(OPERATION.into(), operation)]),
            }]),
            BTreeSet::new(),
            BTreeMap::new(),
        )
        .unwrap();
        let directory =
            std::env::temp_dir().join(format!("edge-auth-bundle-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let manifest_bytes = manifest.canonical_bytes();
        let components = serde_json::to_vec(&[&fact]).unwrap();
        let grants = br#"{"roles":{"operator":["scale:device/record@1.0.0"]}}"#;
        let ingress = b"ingress";
        for (name, bytes) in [
            (RELEASE_MANIFEST_FILE_NAME, manifest_bytes.as_slice()),
            (COMPONENTS_FILE_NAME, components.as_slice()),
            (GRANTS_FILE_NAME, grants.as_slice()),
            (INGRESS_FILE_NAME, ingress.as_slice()),
        ] {
            std::fs::write(directory.join(name), bytes).unwrap();
        }
        let tag = fact.component_digest.strip_prefix("sha256:").unwrap();
        std::fs::write(directory.join(format!("{tag}.wasm")), b"device").unwrap();
        let bundle = serde_json::to_vec(&serde_json::json!({
            "format": BUNDLE_FORMAT,
            "manifest": file_digest(&manifest_bytes),
            "components": file_digest(&components),
            "grants": file_digest(grants),
            "ingress": file_digest(ingress),
        }))
        .unwrap();
        std::fs::write(directory.join(BUNDLE_FILE_NAME), &bundle).unwrap();
        let scope = ReleaseScope::new("tenant-a", "edge").with_applications(BTreeMap::from([(
            "scale".into(),
            wamn_catalog::ApplicationDefinition {
                packages: BTreeSet::from(["scale".into()]),
                connectors: BTreeMap::new(),
                connections: BTreeMap::new(),
            },
        )]));
        let release = EdgeRelease::load(&directory, &file_digest(&bundle), scope)
            .await
            .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        release
    }
}

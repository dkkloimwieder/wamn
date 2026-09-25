//! A platform-published release loaded from a local directory.
//!
//! Publish writes the bundle, and the box never assembles one. The directory
//! holds:
//!
//! - `edge-release.json`: the bundle, which names the SHA-256 of the next
//!   three files.
//! - The canonical serving manifest.
//! - `components.json`: the admitted component facts of the release, which the
//!   cloud host reads from its catalog.
//! - `grants.json`: the permissions of each role ([`Grants`]).
//! - `<sha256>.wasm`: the bytes of each component.
//!
//! The edge configuration pins the bundle digest, as a pod template pins the
//! manifest digest. One pinned digest covers every file, and a later release
//! signature signs that same digest. The loader refuses any file whose bytes do
//! not match.

use std::collections::BTreeSet;
use std::fmt::{self, Write as _};
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use wamn_catalog::{AdmittedComponent, RELEASE_MANIFEST_FILE_NAME};
use wamn_engine::artifact_source::{ArtifactSource as _, LocalComponentSource};
use wamn_engine::release_manifest::{LoadedRelease, validate_component_in_release};

use crate::grants::{GRANTS_FILE_NAME, Grants};

/// The bundle file name inside a release bundle directory.
pub const BUNDLE_FILE_NAME: &str = "edge-release.json";
/// The component facts file name inside a release bundle directory.
pub const COMPONENTS_FILE_NAME: &str = "components.json";
/// The one bundle format this edge reads.
pub const BUNDLE_FORMAT: u32 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    format: u32,
    manifest: String,
    components: String,
    grants: String,
}

/// A release, its component facts, and its grants, each checked against the
/// pinned bundle digest.
#[derive(Debug)]
pub struct EdgeRelease {
    bundle_digest: String,
    release: LoadedRelease,
    components: Vec<AdmittedComponent>,
    grants: Grants,
}

impl EdgeRelease {
    /// Load the bundle in `directory` whose `edge-release.json` has
    /// `bundle_digest`, and read every component body once.
    pub async fn load(directory: &Path, bundle_digest: &str) -> Result<Self, EdgeReleaseError> {
        let bundle_bytes = read_pinned(directory, BUNDLE_FILE_NAME, bundle_digest)?;
        let bundle: Bundle = serde_json::from_slice(&bundle_bytes)
            .map_err(|error| rejected(format!("{BUNDLE_FILE_NAME} is not a bundle: {error}")))?;
        if bundle.format != BUNDLE_FORMAT {
            return Err(rejected(format!(
                "{BUNDLE_FILE_NAME} has format {}, and this edge reads {BUNDLE_FORMAT}",
                bundle.format
            )));
        }

        let manifest_bytes = read_pinned(directory, RELEASE_MANIFEST_FILE_NAME, &bundle.manifest)?;
        let release =
            LoadedRelease::load_canonical_bytes(&manifest_bytes, RELEASE_MANIFEST_FILE_NAME)
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
        for component in &components {
            source.pull_verified(component).await.map_err(|error| {
                EdgeReleaseError::new(EdgeReleaseErrorKind::Mismatch, error.to_string())
            })?;
        }

        let grants_bytes = read_pinned(directory, GRANTS_FILE_NAME, &bundle.grants)?;
        let grants = Grants::parse(&grants_bytes, release.manifest())?;

        Ok(Self {
            bundle_digest: bundle_digest.to_owned(),
            release,
            components,
            grants,
        })
    }

    /// The pinned digest of `edge-release.json`.
    pub fn bundle_digest(&self) -> &str {
        &self.bundle_digest
    }

    /// The loaded serving manifest and its identity.
    pub fn release(&self) -> &LoadedRelease {
        &self.release
    }

    /// The admitted component facts, one per manifest component.
    pub fn components(&self) -> &[AdmittedComponent] {
        &self.components
    }

    /// The permissions of each role.
    pub fn grants(&self) -> &Grants {
        &self.grants
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
    if let Some(missing) = manifest
        .components
        .iter()
        .find(|served| !seen.contains(&(served.package_id.as_str(), served.component.as_str())))
    {
        return Err(rejected(format!(
            "{COMPONENTS_FILE_NAME} has no fact for {}/{}",
            missing.package_id, missing.component
        )));
    }
    Ok(())
}

/// Read `name` under `directory` and refuse bytes whose SHA-256 is not `pinned`.
fn read_pinned(directory: &Path, name: &str, pinned: &str) -> Result<Vec<u8>, EdgeReleaseError> {
    let path = directory.join(name);
    let bytes = std::fs::read(&path).map_err(|error| {
        EdgeReleaseError::new(
            EdgeReleaseErrorKind::Unreadable,
            format!("read {}: {error}", path.display()),
        )
    })?;
    let digest = file_digest(&bytes);
    if digest != pinned {
        return Err(EdgeReleaseError::new(
            EdgeReleaseErrorKind::Mismatch,
            format!("{name} has digest {digest}, and the release pins {pinned}"),
        ));
    }
    Ok(bytes)
}

/// The `sha256:<hex>` digest of exact file bytes.
pub fn file_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity("sha256:".len() + digest.len() * 2);
    output.push_str("sha256:");
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to a string is infallible");
    }
    output
}

/// Stable classification for a refused edge release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeReleaseErrorKind {
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
    kind: EdgeReleaseErrorKind,
    detail: Box<str>,
}

impl EdgeReleaseError {
    pub(crate) fn new(kind: EdgeReleaseErrorKind, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// Return the stable failure class.
    pub fn kind(&self) -> EdgeReleaseErrorKind {
        self.kind
    }
}

impl fmt::Display for EdgeReleaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "edge release refused: {}", self.detail)
    }
}

impl std::error::Error for EdgeReleaseError {}

fn rejected(detail: String) -> EdgeReleaseError {
    EdgeReleaseError::new(EdgeReleaseErrorKind::Rejected, detail)
}

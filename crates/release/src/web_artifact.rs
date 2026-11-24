//! Contracts and archive plans for web publication.

mod plan;
pub use plan::{Scheme, Sink, client_project, content_type, validate_path};

use anyhow::{bail, ensure};
use std::path::Path;
use wamn_digest::canonical_json_bytes;
use wamn_oci_reference::OciTag;

/// The web artifact's metadata, beside `dist/`.
pub const WEB_MANIFEST_FILE: &str = "web.json";
/// The client identity and package contract that accompany the browser files.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebManifest {
    pub client_package: wamn_package::client_package_name::ClientPackageName,
    /// The version from the package's `web/package.json`.
    pub version: String,
    /// `<package id>@<package version>` from the package build record.
    #[serde(with = "package_coordinate_wire")]
    pub package: wamn_package::PackageCoordinate,
    /// `outputs.manifest`, the digest of the compiled canonical `wamn.json`.
    pub contract_digest: wamn_digest::Digest,
}

mod package_coordinate_wire {
    use serde::{Deserialize as _, Deserializer, Serializer};
    use wamn_package::PackageCoordinate;

    pub(super) fn serialize<S: Serializer>(
        value: &PackageCoordinate,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&format_args!(
            "{}@{}",
            value.package_id(),
            value.package_version()
        ))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<PackageCoordinate, D::Error> {
        let value = String::deserialize(deserializer)?;
        crate::publish_release::parse_package(&value).map_err(serde::de::Error::custom)
    }
}

impl WebManifest {
    /// Canonical JSON bytes, independent of field order and whitespace.
    pub fn to_bytes(&self) -> Vec<u8> {
        canonical_json_bytes(&serde_json::to_value(self).expect("web metadata serializes"))
    }
}

#[derive(Debug)]
pub enum WebArtifactErrorType {
    Build,
    Pack,
    Push,
    Pull,
    Upload,
}

/// A web artifact failure with its action, path or reference, and source.
#[derive(Debug)]
pub struct WebArtifactError {
    pub type_: WebArtifactErrorType,
    pub context: String,
    pub source: anyhow::Error,
}

impl std::fmt::Display for WebArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "web artifact {:?} {}: {:#}",
            self.type_, self.context, self.source
        )
    }
}

impl std::error::Error for WebArtifactError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// One deterministic tar and its canonical OCI image manifest.
#[derive(Debug)]
pub struct PackedWebArtifact {
    pub tag: OciTag,
    pub digest: wamn_digest::Digest,
    pub manifest: Vec<u8>,
    pub tar: Vec<u8>,
}

/// Whether the tag was created or already held the same artifact digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebPushDisposition {
    Pushed,
    AlreadyPresent,
}

/// The immutable tag and digest after a successful web push.
#[derive(Debug)]
pub struct PushedWebArtifact {
    pub tag: OciTag,
    pub digest: wamn_digest::Digest,
    pub disposition: WebPushDisposition,
}

/// The package, destination, and completion state of an artifact upload.
#[derive(Debug)]
pub struct UploadedClient {
    pub package: wamn_package::PackageName,
    pub location: String,
    pub written: bool,
}

/// Read digest-pinned archives for a browser artifact upload.
#[async_trait::async_trait]
pub trait WebArtifactReader: Send + Sync {
    fn artifact_base(&self) -> &str;

    async fn read_web(
        &self,
        digest: &wamn_digest::Digest,
    ) -> Result<(WebManifest, Vec<u8>), WebArtifactError>;

    async fn read_platform_ui(&self, digest: &wamn_digest::Digest) -> anyhow::Result<Vec<u8>>;
}

#[cfg(test)]
mod tests {
    use super::Sink;

    #[test]
    fn the_sink_refuses_an_unsupported_scheme() {
        assert!(Sink::parse("https://web").is_err());
    }
}

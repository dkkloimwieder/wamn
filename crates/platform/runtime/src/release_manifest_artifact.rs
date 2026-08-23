//! Shared wire contract for digest-addressed release-manifest artifacts.
//!
//! One layout is written by `wamn-ctl push-release-manifest` and read back by
//! [`ReleaseManifestSource`](crate::release_manifest_source::ReleaseManifestSource),
//! so both the build and the verification of that layout live here. A second
//! copy on either side could drift, and drift in this envelope is exactly what
//! makes a release artifact unrecognizable to the pods that serve it.

use std::fmt;

use oci_client::client::{Config, ImageLayer};
use oci_client::manifest::{OCI_IMAGE_MEDIA_TYPE, OciDescriptor, OciImageManifest};

use crate::component_artifact::{
    ComponentArtifactReference, ComponentArtifactReferenceError, component_artifact_reference,
};

/// OCI artifact type and single-layer media type for canonical format-2 bytes.
pub const RELEASE_MANIFEST_ARTIFACT_MEDIA_TYPE: &str =
    "application/vnd.wamn.release-manifest.v2+json";

/// OCI's required empty config descriptor for a data artifact.
pub const RELEASE_MANIFEST_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";

/// Canonical empty JSON carried by the required OCI config descriptor.
pub const RELEASE_MANIFEST_CONFIG_BYTES: &[u8] = b"{}";

/// Registry, repository, and immutable manifest-digest-derived tag.
pub type ReleaseManifestArtifactReference = ComponentArtifactReference;

/// Refusal to derive an immutable release-manifest artifact reference.
pub type ReleaseManifestArtifactReferenceError = ComponentArtifactReferenceError;

/// Derive the one OCI reference used for a canonical serving manifest.
///
/// `artifact_base` is explicitly `<registry>/<repository>`. The canonical
/// manifest digest is carried as the tag without its `sha256:` prefix because
/// OCI tags cannot contain a colon. No default registry or repository exists.
pub fn release_manifest_artifact_reference(
    artifact_base: &str,
    manifest_digest: &str,
) -> Result<ReleaseManifestArtifactReference, ReleaseManifestArtifactReferenceError> {
    component_artifact_reference(artifact_base, manifest_digest)
}

/// The frozen literal naming which layout invariant an artifact broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseManifestLayoutRefusal(&'static str);

impl ReleaseManifestLayoutRefusal {
    /// Stable literal naming the rejected invariant.
    pub const fn refusal(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for ReleaseManifestLayoutRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for ReleaseManifestLayoutRefusal {}

/// The two descriptors a verified release-manifest artifact carries.
#[derive(Debug)]
pub struct ReleaseManifestArtifactBlobs<'a> {
    /// The single layer holding the canonical format-2 manifest bytes.
    pub layer: &'a OciDescriptor,
    /// The required empty OCI config descriptor.
    pub config: &'a OciDescriptor,
}

/// Build the exact artifact layout carrying one canonical serving manifest.
pub fn release_manifest_artifact_layout(
    canonical_bytes: &[u8],
) -> (ImageLayer, Config, OciImageManifest) {
    let layer = ImageLayer::new(
        canonical_bytes.to_vec(),
        RELEASE_MANIFEST_ARTIFACT_MEDIA_TYPE.to_owned(),
        None,
    );
    let config = Config::new(
        RELEASE_MANIFEST_CONFIG_BYTES.to_vec(),
        RELEASE_MANIFEST_CONFIG_MEDIA_TYPE.to_owned(),
        None,
    );
    let mut manifest = OciImageManifest::build(std::slice::from_ref(&layer), &config, None);
    manifest.media_type = Some(OCI_IMAGE_MEDIA_TYPE.to_owned());
    manifest.artifact_type = Some(RELEASE_MANIFEST_ARTIFACT_MEDIA_TYPE.to_owned());
    (layer, config, manifest)
}

/// Verify one envelope is exactly the layout [`release_manifest_artifact_layout`] writes.
///
/// `expected_size` is `Some` only for a caller that already holds the canonical
/// bytes. A puller does not know the length in advance and learns it from the
/// verified layer descriptor instead; the layer digest still pins the content.
pub fn verify_release_manifest_artifact_layout<'a>(
    manifest: &'a OciImageManifest,
    expected_digest: &str,
    expected_size: Option<usize>,
) -> Result<ReleaseManifestArtifactBlobs<'a>, ReleaseManifestLayoutRefusal> {
    let (_, expected_config, expected_manifest) = release_manifest_artifact_layout(&[]);
    let expected_config_digest = expected_config.sha256_digest();
    if manifest.schema_version != 2
        || manifest.media_type.as_deref() != Some(OCI_IMAGE_MEDIA_TYPE)
        || manifest.artifact_type.as_deref() != Some(RELEASE_MANIFEST_ARTIFACT_MEDIA_TYPE)
        || manifest.subject.is_some()
        || manifest.annotations.is_some()
    {
        return Err(ReleaseManifestLayoutRefusal(
            "release-manifest-artifact-envelope-mismatch",
        ));
    }
    if manifest.layers.len() != 1 {
        return Err(ReleaseManifestLayoutRefusal(
            "release-manifest-artifact-layer-cardinality-mismatch",
        ));
    }
    let layer = &manifest.layers[0];
    let layer_size_mismatch = expected_size
        .is_some_and(|size| layer.size != i64::try_from(size).unwrap_or(i64::MAX))
        || layer.size < 0;
    if layer.media_type != RELEASE_MANIFEST_ARTIFACT_MEDIA_TYPE
        || layer.digest != expected_digest
        || layer_size_mismatch
        || layer.urls.is_some()
        || layer.annotations.is_some()
        || layer.artifact_type.is_some()
    {
        return Err(ReleaseManifestLayoutRefusal(
            "release-manifest-artifact-layer-mismatch",
        ));
    }
    let config = &manifest.config;
    if config.media_type != RELEASE_MANIFEST_CONFIG_MEDIA_TYPE
        || config.digest != expected_config_digest
        || config.size != expected_manifest.config.size
        || config.urls.is_some()
        || config.annotations.is_some()
        || config.artifact_type.is_some()
    {
        return Err(ReleaseManifestLayoutRefusal(
            "release-manifest-artifact-config-mismatch",
        ));
    }
    Ok(ReleaseManifestArtifactBlobs { layer, config })
}

#[cfg(test)]
mod tests {
    use crate::component_artifact::ComponentArtifactReferenceErrorKind;

    use super::*;

    #[test]
    fn release_manifest_artifact_wire_literals_are_pinned() {
        assert_eq!(
            RELEASE_MANIFEST_ARTIFACT_MEDIA_TYPE,
            "application/vnd.wamn.release-manifest.v2+json"
        );
        assert_eq!(
            RELEASE_MANIFEST_CONFIG_MEDIA_TYPE,
            "application/vnd.oci.empty.v1+json"
        );
        assert_eq!(RELEASE_MANIFEST_CONFIG_BYTES, b"{}");
    }

    #[test]
    fn reference_is_derived_from_only_the_explicit_base_and_manifest_digest() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let reference = release_manifest_artifact_reference(
            "registry.wamn-system.svc.cluster.local:5000/wamn/releases",
            &digest,
        )
        .expect("release manifest reference derives");

        assert_eq!(
            reference.registry(),
            "registry.wamn-system.svc.cluster.local:5000"
        );
        assert_eq!(reference.repository(), "wamn/releases");
        assert_eq!(reference.tag(), "a".repeat(64));
        assert_eq!(
            reference.to_string(),
            format!(
                "registry.wamn-system.svc.cluster.local:5000/wamn/releases:{}",
                "a".repeat(64)
            )
        );
    }

    #[test]
    fn mutable_or_ambient_reference_refuses() {
        let digest = format!("sha256:{}", "a".repeat(64));
        for base in [
            "wamn/releases",
            "registry/releases",
            "registry:5000/wamn/releases:latest",
            "https://registry.example/wamn/releases",
        ] {
            assert_eq!(
                release_manifest_artifact_reference(base, &digest)
                    .expect_err("invalid release artifact base refuses")
                    .kind(),
                ComponentArtifactReferenceErrorKind::InvalidBase,
                "accepted {base:?}"
            );
        }
    }
}

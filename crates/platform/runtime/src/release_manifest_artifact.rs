//! Shared wire contract for digest-addressed release-manifest artifacts.

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

//! Digest-verified OCI supply of one release's canonical serving-manifest bytes.
//!
//! This is the read side of the artifact `wamn-ctl push-release-manifest`
//! writes, and it is what makes that artifact the release's desired-state
//! source rather than a write-only copy: a serving process is told its release
//! by an explicit `<registry>/<repository>` base plus the manifest digest its
//! pod template names, and this module returns only bytes whose SHA-256 is that
//! exact digest.
//!
//! # Why the name check that the mount could not make lives here
//!
//! A projected ConfigMap carries no usable binding between the bytes and the
//! name the template asked for — the name inside the container is placed by the
//! same template that mounts the bytes, so comparing them tests the template
//! against itself (see [`crate::release_manifest`]). A registry is a third
//! party: the digest travels in the pod template, the bytes come from the
//! registry, and this module refuses unless they agree. Nothing about the
//! release's identity is derived here, only proven.
//!
//! This module owns artifact transfer alone. Document admission — canonicality,
//! format version, the release pair — stays with
//! [`ReleaseManifestWeld`](crate::release_manifest::ReleaseManifestWeld).

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use wamn_catalog::MAX_SERVING_MANIFEST_BYTES;

use crate::component_admission::component_digest;
use crate::component_artifact::{ComponentArtifactBase, parse_component_artifact_base};
use crate::registry_credentials::read_registry_credentials;
use crate::release_manifest_artifact::verify_release_manifest_artifact_layout;

/// Bound each registry connect/read phase without adding a deployment knob.
const REGISTRY_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Stable classification of a refused release-manifest pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseManifestFetchErrorKind {
    /// The configured base or the named digest cannot form an immutable reference.
    InvalidReference,
    /// The projected registry credential could not be loaded for this registry.
    Credential,
    /// A configured PEM CA bundle could not be read as a trust root.
    TrustAnchor,
    /// The registry or the named artifact is not currently available.
    Unavailable,
    /// The registry answered with bytes or metadata the named digest contradicts.
    Mismatched,
}

/// Contextual refusal from the release-manifest transfer boundary.
pub struct ReleaseManifestFetchError {
    kind: ReleaseManifestFetchErrorKind,
    reference: Option<Box<str>>,
    refusal: &'static str,
}

impl ReleaseManifestFetchError {
    /// Stable refusal class for callers that must not match display text.
    pub fn kind(&self) -> ReleaseManifestFetchErrorKind {
        self.kind
    }

    /// Stable literal naming the exact refused invariant or transfer phase.
    pub fn refusal(&self) -> &'static str {
        self.refusal
    }

    fn invalid_reference() -> Self {
        Self {
            kind: ReleaseManifestFetchErrorKind::InvalidReference,
            reference: None,
            refusal: "release-manifest-artifact-reference-invalid",
        }
    }

    fn credential(refusal: &'static str) -> Self {
        Self {
            kind: ReleaseManifestFetchErrorKind::Credential,
            reference: None,
            refusal,
        }
    }

    fn trust_anchor() -> Self {
        Self {
            kind: ReleaseManifestFetchErrorKind::TrustAnchor,
            reference: None,
            refusal: "release-manifest-artifact-ca-bundle-unreadable",
        }
    }

    fn mismatched(reference: &str, refusal: &'static str) -> Self {
        Self {
            kind: ReleaseManifestFetchErrorKind::Mismatched,
            reference: Some(reference.into()),
            refusal,
        }
    }

    /// Every registry failure is one refusal: this pod does not have its release.
    ///
    /// The transferred content is classified by the explicit layout, size and
    /// digest checks below, never by an upstream transport variant — matching
    /// on that list would bind this refusal taxonomy to an `oci-client` version.
    fn unavailable(reference: &str, refusal: &'static str) -> Self {
        Self {
            kind: ReleaseManifestFetchErrorKind::Unavailable,
            reference: Some(reference.into()),
            refusal,
        }
    }
}

impl fmt::Debug for ReleaseManifestFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The upstream registry error is deliberately omitted: this type is
        // safe to log without emitting response bodies or future auth context.
        formatter
            .debug_struct("ReleaseManifestFetchError")
            .field("kind", &self.kind)
            .field("reference", &self.reference)
            .field("refusal", &self.refusal)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ReleaseManifestFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(reference) = &self.reference {
            write!(
                formatter,
                "release manifest artifact {reference}: {}",
                self.refusal
            )
        } else {
            write!(formatter, "release manifest artifact: {}", self.refusal)
        }
    }
}

impl std::error::Error for ReleaseManifestFetchError {}

/// OCI source returning only canonical manifest bytes the named digest addresses.
///
/// A process pulls its one release once, during construction, and holds the
/// verified bytes in its weld — so this is deliberately not a shared, cloneable
/// service and owns no cache, retry policy or refresh.
pub struct ReleaseManifestSource {
    client: OciClient,
    base: ComponentArtifactBase,
    auth: RegistryAuth,
    /// Kept so [`ReleaseManifestSource::with_ca_paths`] can rebuild the client
    /// without re-reading the credential.
    insecure_registry: bool,
}

impl ReleaseManifestSource {
    /// Configure one release repository from explicit, validated transport facts.
    ///
    /// The credential is the same projected `.dockerconfigjson` the pod already
    /// mounts for component pulls; a release repository on another registry
    /// needs its own entry in that file, not a second knob here.
    pub fn new(
        artifact_base: &str,
        insecure_registry: bool,
        registry_auth_file: &Path,
    ) -> Result<Self, ReleaseManifestFetchError> {
        let base = parse_component_artifact_base(artifact_base)
            .map_err(|_| ReleaseManifestFetchError::invalid_reference())?;
        let credentials = read_registry_credentials(registry_auth_file, base.registry())
            .map_err(|error| ReleaseManifestFetchError::credential(error.refusal()))?;
        let client = registry_client(base.registry(), insecure_registry, Vec::new());
        Ok(Self {
            client,
            base,
            auth: RegistryAuth::Basic(
                credentials.username().to_owned(),
                credentials.password().to_owned(),
            ),
            insecure_registry,
        })
    }

    /// Trust the PEM CA bundles at `paths` for pulls from this release repository.
    ///
    /// This source builds its own `oci-client`, so the process-wide trust roots
    /// a host installs through `wash_runtime::oci::set_extra_ca_certificates` do
    /// not reach it — the same gap
    /// [`ComponentArtifactSourceConfig::with_ca_paths`](crate::component_artifact_source::ComponentArtifactSourceConfig::with_ca_paths)
    /// documents, and the reason a release pull from a registry behind a private
    /// CA fails TLS without this.
    ///
    /// Pass the same paths the host passed to that call, and call it first:
    /// validation lives there, and it refuses an unusable bundle before the
    /// process starts. Empty `paths` leave this source on the compiled-in roots.
    ///
    /// The duplication is measured and ledgered, not an oversight: vanilla
    /// wash-runtime keeps the matching reader `oci::extra_ca_certificates()`
    /// private, and its only public transfer surface pushes and pulls wasm
    /// components — not this artifact's
    /// `application/vnd.wamn.release-manifest.v2+json` layer over an empty
    /// config. See standing trigger 5 in
    /// `docs/architecture/native-alignment-ledger.md` (`wamn-kdhw`).
    pub fn with_ca_paths(mut self, paths: &[PathBuf]) -> Result<Self, ReleaseManifestFetchError> {
        if paths.is_empty() {
            return Ok(self);
        }
        let mut bundles = Vec::with_capacity(paths.len());
        for path in paths {
            bundles
                .push(std::fs::read(path).map_err(|_| ReleaseManifestFetchError::trust_anchor())?);
        }
        self.client = registry_client(self.base.registry(), self.insecure_registry, bundles);
        Ok(self)
    }

    /// Pull the exact canonical bytes this manifest digest addresses.
    pub async fn pull_verified(
        &self,
        manifest_digest: &str,
    ) -> Result<Vec<u8>, ReleaseManifestFetchError> {
        let artifact = self
            .base
            .reference(manifest_digest)
            .map_err(|_| ReleaseManifestFetchError::invalid_reference())?;
        let reference = Reference::with_tag(
            artifact.registry().to_owned(),
            artifact.repository().to_owned(),
            artifact.tag().to_owned(),
        );
        let named = artifact.to_string();

        let (manifest, _) = self
            .client
            .pull_image_manifest(&reference, &self.auth)
            .await
            .map_err(|_| {
                ReleaseManifestFetchError::unavailable(
                    &named,
                    "release-manifest-artifact-manifest-unavailable",
                )
            })?;
        // The config blob is the frozen empty document and its digest is pinned
        // by the layout check, so there is nothing a second round trip could
        // learn. Only the layer carries content.
        let blobs = verify_release_manifest_artifact_layout(&manifest, manifest_digest, None)
            .map_err(|refusal| ReleaseManifestFetchError::mismatched(&named, refusal.refusal()))?;
        if blobs.layer.size > manifest_byte_ceiling() {
            return Err(ReleaseManifestFetchError::mismatched(
                &named,
                "release-manifest-artifact-layer-oversized",
            ));
        }

        let mut canonical_bytes = Vec::new();
        self.client
            .pull_blob(&reference, blobs.layer, &mut canonical_bytes)
            .await
            .map_err(|_| {
                ReleaseManifestFetchError::unavailable(
                    &named,
                    "release-manifest-artifact-body-unavailable",
                )
            })?;
        verify_transferred_body(&canonical_bytes, blobs.layer.size, manifest_digest, &named)?;
        Ok(canonical_bytes)
    }
}

impl fmt::Debug for ReleaseManifestSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReleaseManifestSource")
            .field("registry", &self.base.registry())
            .field("repository", &self.base.repository())
            .finish_non_exhaustive()
    }
}

/// One registry client for this release repository, over these trust roots.
fn registry_client(registry: &str, insecure_registry: bool, ca_bundles: Vec<Vec<u8>>) -> OciClient {
    let protocol = if insecure_registry {
        ClientProtocol::HttpsExcept(vec![registry.to_owned()])
    } else {
        ClientProtocol::Https
    };
    OciClient::new(ClientConfig {
        protocol,
        read_timeout: Some(REGISTRY_IO_TIMEOUT),
        connect_timeout: Some(REGISTRY_IO_TIMEOUT),
        extra_root_certificates: ca_bundles
            .into_iter()
            .map(|data| Certificate {
                encoding: CertificateEncoding::Pem,
                data,
            })
            .collect(),
        ..ClientConfig::default()
    })
}

/// The mint and the mount reader share this ceiling; the puller enforces it too.
fn manifest_byte_ceiling() -> i64 {
    i64::try_from(MAX_SERVING_MANIFEST_BYTES).unwrap_or(i64::MAX)
}

/// Refuse a transferred body the layer descriptor or the named digest contradicts.
///
/// The same shape as
/// [`verify_component_body`](crate::component_artifact_source) next door, and
/// for the same reason: these are the arms that catch a registry or proxy which
/// lies about what it served, so they must be provable without one.
///
/// The size arm is reachable through the wire — nothing upstream compares the
/// declared length to the delivered one — and a stub that overstates it is what
/// kills that mutant. The digest arm is not: MEASURED against `oci-client`
/// 0.17, `pull_blob` digests the streamed body against the layer descriptor it
/// was handed (`client.rs`, `layer_digester`) and
/// [`verify_release_manifest_artifact_layout`] has already pinned that
/// descriptor to the digest the pod template named, so on the `Ok` path the two
/// are equal by construction and no served body can reach this comparison
/// unequal. It is kept as defense in depth against that upstream check being
/// dropped, and a direct call is the only instrument that can hold it.
fn verify_transferred_body(
    canonical_bytes: &[u8],
    descriptor_size: i64,
    expected_digest: &str,
    reference: &str,
) -> Result<(), ReleaseManifestFetchError> {
    if i64::try_from(canonical_bytes.len()).unwrap_or(i64::MAX) != descriptor_size {
        return Err(ReleaseManifestFetchError::mismatched(
            reference,
            "release-manifest-artifact-body-size-mismatch",
        ));
    }
    // `component_digest` is a plain `sha256:<hex>` over bytes; a serving
    // manifest's identity is that same function over its canonical encoding, so
    // this proves the transferred body against the pod template's name rather
    // than against the registry's own bookkeeping.
    if component_digest(canonical_bytes) != expected_digest {
        return Err(ReleaseManifestFetchError::mismatched(
            reference,
            "release-manifest-artifact-body-digest-mismatch",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFERENCE: &str = "registry.example:5000/wamn/releases:tag";

    #[test]
    fn a_body_the_layer_descriptor_overstates_refuses_as_a_size_mismatch() {
        let canonical = br#"{"format-version":2}"#;
        let exact = i64::try_from(canonical.len()).expect("fixture length fits");

        verify_transferred_body(canonical, exact, &component_digest(canonical), REFERENCE)
            .expect("a body served exactly as the descriptor declares verifies");

        let error = verify_transferred_body(
            canonical,
            exact + 1,
            &component_digest(canonical),
            REFERENCE,
        )
        .expect_err("a descriptor declaring more bytes than were served refuses");

        assert_eq!(error.kind(), ReleaseManifestFetchErrorKind::Mismatched);
        assert_eq!(
            error.refusal(),
            "release-manifest-artifact-body-size-mismatch"
        );
    }

    #[test]
    fn a_body_the_named_digest_does_not_address_refuses_as_a_digest_mismatch() {
        let canonical = br#"{"format-version":2}"#;
        // Same length as the named bytes, so the size arm cannot be what fires.
        let served = br#"{"Format-version":2}"#;
        assert_eq!(served.len(), canonical.len());
        let exact = i64::try_from(served.len()).expect("fixture length fits");

        verify_transferred_body(served, exact, &component_digest(served), REFERENCE)
            .expect("a body the descriptor and the name both address verifies");

        let error = verify_transferred_body(served, exact, &component_digest(canonical), REFERENCE)
            .expect_err("a body the named digest does not address refuses");

        assert_eq!(error.kind(), ReleaseManifestFetchErrorKind::Mismatched);
        assert_eq!(
            error.refusal(),
            "release-manifest-artifact-body-digest-mismatch"
        );
    }
}

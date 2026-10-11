//! Push canonical format-1 serving-manifest bytes as one OCI data artifact.
//!
//! The manifest's RFC 8785 SHA-256 identity derives its immutable OCI tag. An
//! exact retry pulls and verifies the existing artifact and performs no push.
//! A tag holding any other layout or bytes refuses instead of being replaced.
//!
//! `publish-release` pushes the bytes it composed and recorded
//! (docs/plan/platform-deploy.md §8.1).

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol};
use oci_client::manifest::OciImageManifest;
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use wamn_catalog::{ManifestDigest, ServingManifest, ServingRelease};
use wamn_package_artifact::read_ca_bundles;
use wamn_package_artifact::registry_credentials::read_registry_push_credentials;
pub(crate) use wamn_package_artifact::{artifact_is_absent, registry_auth};
use wamn_runtime::release_manifest_artifact::{
    RELEASE_MANIFEST_CONFIG_BYTES, ReleaseManifestArtifactBlobs, release_manifest_artifact_layout,
    release_manifest_artifact_reference, verify_release_manifest_artifact_layout,
};

/// Bound each registry connect/read phase without adding a deployment knob.
const REGISTRY_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Stable prefix every release-manifest OCI refusal renders with.
pub const RELEASE_MANIFEST_PUBLISH_REFUSAL: &str = "release-manifest-publish-refused";

/// Stable classification of a release-manifest publication refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseManifestPushErrorType {
    Document,
    Reference,
    Credential,
    TrustAnchor,
    Transport,
    Conflict,
}

impl ReleaseManifestPushErrorType {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Reference => "reference",
            Self::Credential => "credential",
            Self::TrustAnchor => "trust-anchor",
            Self::Transport => "transport",
            Self::Conflict => "conflict",
        }
    }
}

/// Contextual refusal from the release-manifest OCI boundary.
#[derive(Debug)]
pub struct ReleaseManifestPushError {
    type_: ReleaseManifestPushErrorType,
    refusal: &'static str,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ReleaseManifestPushError {
    /// Stable refusal class for callers that must not match display text.
    pub const fn error_type(&self) -> ReleaseManifestPushErrorType {
        self.type_
    }

    /// Stable literal naming the rejected invariant.
    pub const fn refusal(&self) -> &'static str {
        self.refusal
    }

    fn new(
        type_: ReleaseManifestPushErrorType,
        refusal: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            type_,
            refusal,
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source(
        type_: ReleaseManifestPushErrorType,
        refusal: &'static str,
        detail: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            type_,
            refusal,
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for ReleaseManifestPushError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{RELEASE_MANIFEST_PUBLISH_REFUSAL} ({}; {}): {}",
            self.type_.as_str(),
            self.refusal,
            self.detail
        )
    }
}

impl std::error::Error for ReleaseManifestPushError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// Whether this invocation created the artifact or matched an exact retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseManifestPushDisposition {
    Pushed,
    AlreadyPresent,
}

/// Verified result of pushing one canonical serving manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushedReleaseManifest {
    pub digest: ManifestDigest,
    pub disposition: ReleaseManifestPushDisposition,
    /// Effective-release coordinate carried by the pushed bytes.
    pub release: ServingRelease,
}

/// Push canonical format-1 bytes or check that their exact artifact already exists.
pub async fn push_manifest_bytes(
    canonical_bytes: &[u8],
    artifact_base: &str,
    insecure_registry: bool,
    oci_ca_paths: &[PathBuf],
    registry_auth_file: &Path,
) -> Result<PushedReleaseManifest, ReleaseManifestPushError> {
    let (admitted, digest) =
        ServingManifest::from_canonical_bytes(canonical_bytes).map_err(|source| {
            ReleaseManifestPushError::with_source(
                ReleaseManifestPushErrorType::Document,
                "release-manifest-document-refused",
                "input is not canonical format-1 ServingManifest JSON",
                source,
            )
        })?;
    let release = admitted.release;
    let artifact =
        release_manifest_artifact_reference(artifact_base, digest.as_str()).map_err(|source| {
            ReleaseManifestPushError::with_source(
                ReleaseManifestPushErrorType::Reference,
                "release-manifest-artifact-reference-refused",
                "artifact base or manifest digest cannot form an immutable OCI reference",
                source,
            )
        })?;
    let credentials = read_registry_push_credentials(registry_auth_file, artifact.registry())
        .map_err(|source| {
            ReleaseManifestPushError::with_source(
                ReleaseManifestPushErrorType::Credential,
                "release-manifest-registry-credential-refused",
                format!("load push credential for registry {}", artifact.registry()),
                source,
            )
        })?;
    let reference = Reference::with_tag(
        artifact.registry().to_owned(),
        artifact.repository().to_owned(),
        artifact.tag().to_owned(),
    );
    let ca_bundles = read_ca_bundles(oci_ca_paths).map_err(|source| {
        ReleaseManifestPushError::with_source(
            ReleaseManifestPushErrorType::TrustAnchor,
            "release-manifest-ca-bundle-unreadable",
            format!("read CA bundles for registry {}", artifact.registry()),
            source,
        )
    })?;
    let client = registry_client(artifact.registry(), insecure_registry, ca_bundles)?;
    let auth = registry_auth(credentials.as_ref());

    if probe_exact_artifact(&client, &reference, &auth, canonical_bytes, &digest).await? {
        return Ok(PushedReleaseManifest {
            digest,
            disposition: ReleaseManifestPushDisposition::AlreadyPresent,
            release,
        });
    }

    let (layer, config, manifest) = release_manifest_artifact_layout(canonical_bytes);
    client
        .push(
            &reference,
            std::slice::from_ref(&layer),
            config,
            &auth,
            Some(manifest),
        )
        .await
        .map_err(|source| {
            ReleaseManifestPushError::with_source(
                ReleaseManifestPushErrorType::Transport,
                "release-manifest-artifact-push-failed",
                format!("push release-manifest artifact {reference}"),
                source,
            )
        })?;

    if !probe_exact_artifact(&client, &reference, &auth, canonical_bytes, &digest).await? {
        return Err(ReleaseManifestPushError::new(
            ReleaseManifestPushErrorType::Transport,
            "release-manifest-artifact-not-visible",
            format!("pushed release-manifest artifact {reference} is not readable"),
        ));
    }
    Ok(PushedReleaseManifest {
        digest,
        disposition: ReleaseManifestPushDisposition::Pushed,
        release,
    })
}

/// One registry client for this push, built here rather than borrowed from
/// `wash_runtime::oci`.
///
/// That module exposes no client-construction seam: its transfer surface is
/// `pull_component`/`push_component`, both fixed to `WASM_LAYER_MEDIA_TYPE` and
/// a `WasmConfig` blob, and `push_component` refuses non-wasm bytes outright
/// through `wit_component::decode_reader`. A release manifest is canonical
/// JSON. This path also needs `HttpsExcept` for a single insecure registry and
/// `OciErrorCode::ManifestUnknown` discrimination for the exact-retry probe,
/// neither of which survives that API. See standing trigger 5 in
/// `docs/architecture/native-alignment.md#retained-wamn-implementations` (`wamn-kdhw`).
///
/// It is built through `TryFrom`, not `Client::new`: that constructor answers a
/// rejected configuration with a warning and a wholly default client, which
/// drops the trust roots, the protocol and the timeouts and turns an unusable
/// CA bundle into a confusing TLS failure on the push itself.
pub(crate) fn registry_client(
    registry: &str,
    insecure_registry: bool,
    ca_bundles: Vec<Vec<u8>>,
) -> Result<OciClient, ReleaseManifestPushError> {
    let protocol = if insecure_registry {
        ClientProtocol::HttpsExcept(vec![registry.to_owned()])
    } else {
        ClientProtocol::Https
    };
    OciClient::try_from(ClientConfig {
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
    .map_err(|source| {
        ReleaseManifestPushError::with_source(
            ReleaseManifestPushErrorType::TrustAnchor,
            "release-manifest-registry-client-unusable",
            format!("build the push client for registry {registry}"),
            source,
        )
    })
}

async fn probe_exact_artifact(
    client: &OciClient,
    reference: &Reference,
    auth: &RegistryAuth,
    expected_bytes: &[u8],
    expected_digest: &ManifestDigest,
) -> Result<bool, ReleaseManifestPushError> {
    let (manifest, _) = match client.pull_image_manifest(reference, auth).await {
        Ok(found) => found,
        Err(source) if artifact_is_absent(&source) => return Ok(false),
        Err(source) => {
            return Err(ReleaseManifestPushError::with_source(
                ReleaseManifestPushErrorType::Transport,
                "release-manifest-artifact-probe-failed",
                format!("probe release-manifest artifact {reference}"),
                source,
            ));
        }
    };
    let verified = verify_manifest_layout(
        &manifest,
        expected_digest.as_str(),
        expected_bytes.len(),
        reference,
    )?;

    let mut body = Vec::new();
    client
        .pull_blob(reference, verified.layer, &mut body)
        .await
        .map_err(|source| {
            ReleaseManifestPushError::with_source(
                ReleaseManifestPushErrorType::Transport,
                "release-manifest-artifact-body-unavailable",
                format!("pull release-manifest body {reference}"),
                source,
            )
        })?;
    if body != expected_bytes {
        return Err(conflict(
            reference,
            "release-manifest-artifact-body-mismatch",
        ));
    }

    let mut config = Vec::new();
    client
        .pull_blob(reference, verified.config, &mut config)
        .await
        .map_err(|source| {
            ReleaseManifestPushError::with_source(
                ReleaseManifestPushErrorType::Transport,
                "release-manifest-artifact-config-unavailable",
                format!("pull release-manifest config {reference}"),
                source,
            )
        })?;
    if config != RELEASE_MANIFEST_CONFIG_BYTES {
        return Err(conflict(
            reference,
            "release-manifest-artifact-config-body-mismatch",
        ));
    }
    Ok(true)
}

/// Translate the shared layout contract's refusal into this boundary's error.
fn verify_manifest_layout<'a>(
    manifest: &'a OciImageManifest,
    expected_digest: &str,
    expected_size: usize,
    reference: &Reference,
) -> Result<ReleaseManifestArtifactBlobs<'a>, ReleaseManifestPushError> {
    verify_release_manifest_artifact_layout(manifest, expected_digest, Some(expected_size))
        .map_err(|refusal| conflict(reference, refusal.refusal()))
}

fn conflict(reference: &Reference, refusal: &'static str) -> ReleaseManifestPushError {
    ReleaseManifestPushError::new(
        ReleaseManifestPushErrorType::Conflict,
        refusal,
        format!("existing release-manifest artifact {reference} is not exact"),
    )
}

#[cfg(test)]
mod tests {
    use wamn_runtime::component_artifact_source::OCI_CA_PATHS_ENV;
    use wamn_runtime::release_manifest_artifact::RELEASE_MANIFEST_ARTIFACT_MEDIA_TYPE;

    use super::*;

    const CANONICAL_MANIFEST: &[u8] = br#"{"attachments":{},"components":[{"descriptor":{"component":"http-request","component-digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","connections":[],"effects":[],"imports":[],"imports-fingerprint":"","interface-version":"0.1","operations":{}},"operations":{"wamn:node/handler@0.1.0":{}},"package-id":"orders"}],"format-version":7,"release":{"packages":[{"package-id":"orders","package-version":"1.0.0"}]},"routes":[],"workflow":{"wirings":[{"graph-hash":"sha256:3333333333333333333333333333333333333333333333333333333333333333","package-id":"orders","wiring-id":"orders","wiring-version":1}]}}"#;

    fn fixture_reference() -> Reference {
        Reference::with_tag(
            "registry.example".to_owned(),
            "wamn/releases".to_owned(),
            "a".repeat(64),
        )
    }

    #[test]
    fn exact_layout_carries_only_canonical_manifest_bytes() {
        let (_, digest) = ServingManifest::from_canonical_bytes(CANONICAL_MANIFEST)
            .expect("fixture is a canonical format-1 manifest");
        let (layer, config, manifest) = release_manifest_artifact_layout(CANONICAL_MANIFEST);
        let verified = verify_manifest_layout(
            &manifest,
            digest.as_str(),
            CANONICAL_MANIFEST.len(),
            &fixture_reference(),
        )
        .expect("push layout verifies");

        assert_eq!(&layer.data[..], CANONICAL_MANIFEST);
        assert_eq!(&config.data[..], RELEASE_MANIFEST_CONFIG_BYTES);
        assert_eq!(verified.layer.digest, digest.as_str());
        assert_eq!(manifest.layers.len(), 1);
    }

    #[test]
    fn unsupported_format_and_noncanonical_documents_refuse_before_transport() {
        let unsupported = std::str::from_utf8(CANONICAL_MANIFEST)
            .expect("fixture is UTF-8")
            .replacen("\"format-version\":7", "\"format-version\":6", 1);
        let unsupported = ServingManifest::from_canonical_bytes(unsupported.as_bytes())
            .expect_err("format six refuses");
        assert!(format!("{unsupported}").contains("unsupported-serving-manifest-version"));

        let mut indented = CANONICAL_MANIFEST.to_vec();
        indented.insert(1, b' ');
        assert!(ServingManifest::from_canonical_bytes(&indented).is_err());
    }

    #[test]
    fn wrong_or_multi_layer_layout_refuses_as_conflict() {
        let (_, digest) = ServingManifest::from_canonical_bytes(CANONICAL_MANIFEST)
            .expect("fixture is a canonical format-1 manifest");
        let (_, _, mut manifest) = release_manifest_artifact_layout(CANONICAL_MANIFEST);
        manifest.layers[0].media_type = "application/octet-stream".to_owned();
        let error = verify_manifest_layout(
            &manifest,
            digest.as_str(),
            CANONICAL_MANIFEST.len(),
            &fixture_reference(),
        )
        .expect_err("foreign layer layout refuses");
        assert_eq!(error.error_type(), ReleaseManifestPushErrorType::Conflict);
        assert_eq!(error.refusal(), "release-manifest-artifact-layer-mismatch");

        let duplicate = manifest.layers[0].clone();
        manifest.layers[0].media_type = RELEASE_MANIFEST_ARTIFACT_MEDIA_TYPE.to_owned();
        manifest.layers.push(duplicate);
        let error = verify_manifest_layout(
            &manifest,
            digest.as_str(),
            CANONICAL_MANIFEST.len(),
            &fixture_reference(),
        )
        .expect_err("multiple layers refuse");
        assert_eq!(
            error.refusal(),
            "release-manifest-artifact-layer-cardinality-mismatch"
        );
    }

    #[tokio::test]
    #[ignore = "requires: WAMN_RELEASE_MANIFEST_ARTIFACT_BASE, WAMN_REGISTRY_AUTH_FILE"]
    async fn pushing_same_bytes_uploads_nothing() {
        wamn_test_postgres::require_prerequisites(&[
            "WAMN_RELEASE_MANIFEST_ARTIFACT_BASE",
            "WAMN_REGISTRY_AUTH_FILE",
        ]);
        let artifact_base = std::env::var("WAMN_RELEASE_MANIFEST_ARTIFACT_BASE")
            .expect("set WAMN_RELEASE_MANIFEST_ARTIFACT_BASE to a disposable repository");
        let registry_auth_file = std::env::var("WAMN_REGISTRY_AUTH_FILE")
            .expect("set WAMN_REGISTRY_AUTH_FILE to its Docker config credential");
        // A CA names a TLS registry; without one the registry stays plain HTTP.
        let oci_ca_paths: Vec<PathBuf> = std::env::var(OCI_CA_PATHS_ENV)
            .map(|paths| paths.split(',').map(PathBuf::from).collect())
            .unwrap_or_default();
        let insecure_registry = oci_ca_paths.is_empty();

        let first = push_manifest_bytes(
            CANONICAL_MANIFEST,
            &artifact_base,
            insecure_registry,
            &oci_ca_paths,
            Path::new(&registry_auth_file),
        )
        .await
        .expect("first publication converges");
        let retry = push_manifest_bytes(
            CANONICAL_MANIFEST,
            &artifact_base,
            insecure_registry,
            &oci_ca_paths,
            Path::new(&registry_auth_file),
        )
        .await
        .expect("exact retry converges");

        assert_eq!(first.digest, retry.digest);
        assert_eq!(
            retry.disposition,
            ReleaseManifestPushDisposition::AlreadyPresent
        );
    }
}

//! Publish canonical format-2 serving-manifest bytes as one OCI data artifact.
//!
//! The manifest's RFC 8785 SHA-256 identity derives its immutable OCI tag. An
//! exact retry pulls and verifies the existing artifact and performs no push.
//! A tag holding any other layout or bytes refuses instead of being replaced.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use clap::Args;
use oci_client::client::{ClientConfig, ClientProtocol};
use oci_client::errors::{OciDistributionError, OciErrorCode};
use oci_client::manifest::OciImageManifest;
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use wamn_catalog::{ManifestDigest, ServingManifest};
use wamn_runtime::registry_credentials::{RegistryCredentials, read_registry_credentials};
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
pub enum ReleaseManifestPublishErrorKind {
    Document,
    Reference,
    Credential,
    Transport,
    Conflict,
}

impl ReleaseManifestPublishErrorKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Reference => "reference",
            Self::Credential => "credential",
            Self::Transport => "transport",
            Self::Conflict => "conflict",
        }
    }
}

/// Contextual refusal from the release-manifest OCI boundary.
#[derive(Debug)]
pub struct ReleaseManifestPublishError {
    kind: ReleaseManifestPublishErrorKind,
    refusal: &'static str,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ReleaseManifestPublishError {
    /// Stable refusal class for callers that must not match display text.
    pub const fn kind(&self) -> ReleaseManifestPublishErrorKind {
        self.kind
    }

    /// Stable literal naming the rejected invariant.
    pub const fn refusal(&self) -> &'static str {
        self.refusal
    }

    fn new(
        kind: ReleaseManifestPublishErrorKind,
        refusal: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            refusal,
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source(
        kind: ReleaseManifestPublishErrorKind,
        refusal: &'static str,
        detail: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            refusal,
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for ReleaseManifestPublishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{RELEASE_MANIFEST_PUBLISH_REFUSAL} ({}; {}): {}",
            self.kind.as_str(),
            self.refusal,
            self.detail
        )
    }
}

impl std::error::Error for ReleaseManifestPublishError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// Whether this invocation created the artifact or proved an exact retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseManifestPublishDisposition {
    Pushed,
    AlreadyPresent,
}

/// Verified result of publishing one canonical serving manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedReleaseManifest {
    pub digest: ManifestDigest,
    pub disposition: ReleaseManifestPublishDisposition,
}

/// Arguments for the release-manifest distribution copy.
#[derive(Debug, Args)]
pub struct PushReleaseManifestArgs {
    /// File containing the exact canonical format-2 ServingManifest JSON.
    #[arg(long)]
    pub manifest: PathBuf,

    /// Explicit `<registry>/<repository>` base for release manifests.
    #[arg(long)]
    pub artifact_base: String,

    /// Projected `.dockerconfigjson` file carrying the push credential.
    #[arg(long, env = "WAMN_REGISTRY_AUTH_FILE")]
    pub registry_auth_file: PathBuf,

    /// Use plain HTTP for exactly the registry in `--artifact-base`.
    #[arg(long, default_value_t = false)]
    pub insecure_registry: bool,
}

/// Publish one canonical release-manifest file and print its content digest.
pub async fn run(args: PushReleaseManifestArgs) -> anyhow::Result<()> {
    let canonical_bytes = std::fs::read(&args.manifest)
        .with_context(|| format!("read serving manifest {}", args.manifest.display()))?;
    let published = publish_release_manifest(
        &canonical_bytes,
        &args.artifact_base,
        args.insecure_registry,
        &args.registry_auth_file,
    )
    .await?;
    println!("{}", published.digest);
    Ok(())
}

/// Publish canonical v2 bytes or prove that their exact artifact already exists.
pub async fn publish_release_manifest(
    canonical_bytes: &[u8],
    artifact_base: &str,
    insecure_registry: bool,
    registry_auth_file: &Path,
) -> Result<PublishedReleaseManifest, ReleaseManifestPublishError> {
    let (_, digest) = ServingManifest::from_canonical_bytes(canonical_bytes).map_err(|source| {
        ReleaseManifestPublishError::with_source(
            ReleaseManifestPublishErrorKind::Document,
            "release-manifest-document-refused",
            "input is not canonical format-2 ServingManifest JSON",
            source,
        )
    })?;
    let artifact =
        release_manifest_artifact_reference(artifact_base, digest.as_str()).map_err(|source| {
            ReleaseManifestPublishError::with_source(
                ReleaseManifestPublishErrorKind::Reference,
                "release-manifest-artifact-reference-refused",
                "artifact base or manifest digest cannot form an immutable OCI reference",
                source,
            )
        })?;
    let credentials =
        read_registry_credentials(registry_auth_file, artifact.registry()).map_err(|source| {
            ReleaseManifestPublishError::with_source(
                ReleaseManifestPublishErrorKind::Credential,
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
    let client = registry_client(artifact.registry(), insecure_registry);
    let auth = registry_auth(&credentials);

    if probe_exact_artifact(&client, &reference, &auth, canonical_bytes, &digest).await? {
        return Ok(PublishedReleaseManifest {
            digest,
            disposition: ReleaseManifestPublishDisposition::AlreadyPresent,
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
            ReleaseManifestPublishError::with_source(
                ReleaseManifestPublishErrorKind::Transport,
                "release-manifest-artifact-push-failed",
                format!("push release-manifest artifact {reference}"),
                source,
            )
        })?;

    if !probe_exact_artifact(&client, &reference, &auth, canonical_bytes, &digest).await? {
        return Err(ReleaseManifestPublishError::new(
            ReleaseManifestPublishErrorKind::Transport,
            "release-manifest-artifact-not-visible",
            format!("pushed release-manifest artifact {reference} is not readable"),
        ));
    }
    Ok(PublishedReleaseManifest {
        digest,
        disposition: ReleaseManifestPublishDisposition::Pushed,
    })
}

fn registry_client(registry: &str, insecure_registry: bool) -> OciClient {
    let protocol = if insecure_registry {
        ClientProtocol::HttpsExcept(vec![registry.to_owned()])
    } else {
        ClientProtocol::Https
    };
    OciClient::new(ClientConfig {
        protocol,
        read_timeout: Some(REGISTRY_IO_TIMEOUT),
        connect_timeout: Some(REGISTRY_IO_TIMEOUT),
        ..ClientConfig::default()
    })
}

fn registry_auth(credentials: &RegistryCredentials) -> RegistryAuth {
    RegistryAuth::Basic(
        credentials.username().to_owned(),
        credentials.password().to_owned(),
    )
}

async fn probe_exact_artifact(
    client: &OciClient,
    reference: &Reference,
    auth: &RegistryAuth,
    expected_bytes: &[u8],
    expected_digest: &ManifestDigest,
) -> Result<bool, ReleaseManifestPublishError> {
    let (manifest, _) = match client.pull_image_manifest(reference, auth).await {
        Ok(found) => found,
        Err(source) if artifact_is_absent(&source) => return Ok(false),
        Err(source) => {
            return Err(ReleaseManifestPublishError::with_source(
                ReleaseManifestPublishErrorKind::Transport,
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
            ReleaseManifestPublishError::with_source(
                ReleaseManifestPublishErrorKind::Transport,
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
            ReleaseManifestPublishError::with_source(
                ReleaseManifestPublishErrorKind::Transport,
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
) -> Result<ReleaseManifestArtifactBlobs<'a>, ReleaseManifestPublishError> {
    verify_release_manifest_artifact_layout(manifest, expected_digest, Some(expected_size))
        .map_err(|refusal| conflict(reference, refusal.refusal()))
}

fn artifact_is_absent(error: &OciDistributionError) -> bool {
    match error {
        OciDistributionError::ImageManifestNotFoundError(_) => true,
        OciDistributionError::RegistryError { envelope, .. } => {
            !envelope.errors.is_empty()
                && envelope.errors.iter().all(|error| {
                    matches!(
                        error.code,
                        OciErrorCode::ManifestUnknown
                            | OciErrorCode::NameUnknown
                            | OciErrorCode::NotFound
                    )
                })
        }
        OciDistributionError::ServerError { code: 404, .. } => true,
        _ => false,
    }
}

fn conflict(reference: &Reference, refusal: &'static str) -> ReleaseManifestPublishError {
    ReleaseManifestPublishError::new(
        ReleaseManifestPublishErrorKind::Conflict,
        refusal,
        format!("existing release-manifest artifact {reference} is not exact"),
    )
}

#[cfg(test)]
mod tests {
    use wamn_runtime::release_manifest_artifact::RELEASE_MANIFEST_ARTIFACT_MEDIA_TYPE;

    use super::*;

    const CANONICAL_MANIFEST: &[u8] = br#"{"attachments":{"orders-http":{"auth-policy":{"mode":"none"},"definition":{"id":"orders-http","kind":"http","run-deadline-ms":30000},"definition-hash":"sha256:5555555555555555555555555555555555555555555555555555555555555555","kind":"http","wiring-id":"orders","wiring-version":1}},"components":[{"component":"http-request","digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","interface-version":"0.1"}],"format-version":2,"registrations":{},"release":{"catalog-id":"orders","catalog-version":1,"environment":"prod","tenant-id":"tenant-a"},"wirings":[{"graph-hash":"sha256:3333333333333333333333333333333333333333333333333333333333333333","wiring-id":"orders","wiring-version":1}]}"#;

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
            .expect("fixture is a canonical v2 manifest");
        let (layer, config, manifest) = release_manifest_artifact_layout(CANONICAL_MANIFEST);
        let verified = verify_manifest_layout(
            &manifest,
            digest.as_str(),
            CANONICAL_MANIFEST.len(),
            &fixture_reference(),
        )
        .expect("publisher layout verifies");

        assert_eq!(&layer.data[..], CANONICAL_MANIFEST);
        assert_eq!(&config.data[..], RELEASE_MANIFEST_CONFIG_BYTES);
        assert_eq!(verified.layer.digest, digest.as_str());
        assert_eq!(manifest.layers.len(), 1);
    }

    #[test]
    fn format_one_and_noncanonical_documents_refuse_before_transport() {
        let legacy = std::str::from_utf8(CANONICAL_MANIFEST)
            .expect("fixture is UTF-8")
            .replacen("\"format-version\":2", "\"format-version\":1", 1);
        let legacy = ServingManifest::from_canonical_bytes(legacy.as_bytes())
            .expect_err("format one refuses");
        assert!(format!("{legacy}").contains("unsupported-serving-manifest-version"));

        let mut indented = CANONICAL_MANIFEST.to_vec();
        indented.insert(1, b' ');
        assert!(ServingManifest::from_canonical_bytes(&indented).is_err());
    }

    #[test]
    fn wrong_or_multi_layer_layout_refuses_as_conflict() {
        let (_, digest) = ServingManifest::from_canonical_bytes(CANONICAL_MANIFEST)
            .expect("fixture is a canonical v2 manifest");
        let (_, _, mut manifest) = release_manifest_artifact_layout(CANONICAL_MANIFEST);
        manifest.layers[0].media_type = "application/octet-stream".to_owned();
        let error = verify_manifest_layout(
            &manifest,
            digest.as_str(),
            CANONICAL_MANIFEST.len(),
            &fixture_reference(),
        )
        .expect_err("foreign layer layout refuses");
        assert_eq!(error.kind(), ReleaseManifestPublishErrorKind::Conflict);
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
    #[ignore = "requires a disposable authenticated registry"]
    async fn production_publisher_exact_retry_is_a_no_push() {
        let artifact_base = std::env::var("WAMN_RELEASE_MANIFEST_ARTIFACT_BASE")
            .expect("set WAMN_RELEASE_MANIFEST_ARTIFACT_BASE to a disposable repository");
        let registry_auth_file = std::env::var("WAMN_REGISTRY_AUTH_FILE")
            .expect("set WAMN_REGISTRY_AUTH_FILE to its Docker config credential");

        let first = publish_release_manifest(
            CANONICAL_MANIFEST,
            &artifact_base,
            true,
            Path::new(&registry_auth_file),
        )
        .await
        .expect("first publication converges");
        let retry = publish_release_manifest(
            CANONICAL_MANIFEST,
            &artifact_base,
            true,
            Path::new(&registry_auth_file),
        )
        .await
        .expect("exact retry converges");

        assert_eq!(first.digest, retry.digest);
        assert_eq!(
            retry.disposition,
            ReleaseManifestPublishDisposition::AlreadyPresent
        );
    }
}

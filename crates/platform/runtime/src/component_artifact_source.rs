//! Digest-verified OCI supply of admitted component bytes.
//!
//! This module owns only artifact transfer. Callers provide a complete admitted
//! component fact and explicit registry configuration; the source derives the
//! immutable reference, verifies the manifest and both blobs, and returns only
//! component bytes proven against that fact. It owns no catalog read, cache,
//! instance pool, router behavior, or retry policy.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol};
use oci_client::errors::OciDistributionError;
use oci_client::manifest::{OciDescriptor, OciImageManifest};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use wamn_catalog::AdmittedComponent;

use crate::component_admission::component_digest;
use crate::component_artifact::{
    ComponentArtifactBase, ComponentArtifactReferenceError, component_artifact_config_bytes,
    component_artifact_layout, parse_component_artifact_base,
};
use crate::registry_credentials::{
    RegistryCredentials, RegistryCredentialsError, read_registry_credentials,
};

/// Explicit, validated configuration for one component artifact repository.
#[derive(Clone, PartialEq, Eq)]
pub struct ComponentArtifactSourceConfig {
    base: ComponentArtifactBase,
    insecure_registry: bool,
    fetch_timeout: Duration,
    credentials: Option<RegistryCredentials>,
    /// PEM CA bundles this source trusts on top of the compiled-in roots, kept
    /// as read bytes rather than `oci_client::client::Certificate` so the
    /// configuration stays comparable.
    ca_bundles: Vec<Vec<u8>>,
}

impl ComponentArtifactSourceConfig {
    /// Validate one explicit `<registry>/<repository>` source configuration.
    pub fn new(
        artifact_base: &str,
        insecure_registry: bool,
        fetch_timeout: Duration,
    ) -> Result<Self, ComponentArtifactReferenceError> {
        Ok(Self {
            base: parse_component_artifact_base(artifact_base)?,
            insecure_registry,
            fetch_timeout,
            credentials: None,
            ca_bundles: Vec::new(),
        })
    }

    /// Trust the PEM CA bundles at `paths` for pulls from this source.
    ///
    /// This source builds its own `oci-client`, so the process-wide trust roots
    /// a host installs through `wash_runtime::oci::set_extra_ca_certificates`
    /// do not reach it. Without this it sees only the roots `oci-client`
    /// compiles in, and an in-cluster registry behind a private CA is
    /// unreachable short of dropping verification altogether.
    ///
    /// Pass the same paths the host passed to that call, and call it first:
    /// validation lives there, and it refuses a bundle that is unreadable or
    /// unusable as a trust root rather than starting a host that will reject
    /// every pull. That refusal is load-bearing for this side too, because
    /// `oci-client` builds its client through `Client::new`, which logs and
    /// falls back to a wholly default configuration when a certificate fails to
    /// parse — discarding the registry protocol and the timeouts along with the
    /// trust roots, and leaving only a warning to say so.
    ///
    /// Empty `paths` leave this source on the compiled-in roots.
    pub fn with_ca_paths(mut self, paths: &[PathBuf]) -> Result<Self, ComponentArtifactCaError> {
        for path in paths {
            let bundle = std::fs::read(path)
                .map_err(|source| ComponentArtifactCaError::unreadable(path, source))?;
            self.ca_bundles.push(bundle);
        }
        Ok(self)
    }

    /// Authenticate pulls with one complete credential for this exact registry.
    pub fn with_credentials(mut self, credentials: RegistryCredentials) -> Self {
        self.credentials = Some(credentials);
        self
    }

    /// Load this source's credential from a projected Docker config file.
    pub fn with_registry_auth_file(
        self,
        path: &std::path::Path,
    ) -> Result<Self, RegistryCredentialsError> {
        let credentials = read_registry_credentials(path, self.base.registry())?;
        Ok(self.with_credentials(credentials))
    }
}

impl fmt::Debug for ComponentArtifactSourceConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComponentArtifactSourceConfig")
            .field("registry", &self.base.registry())
            .field("repository", &self.base.repository())
            .field("insecure_registry", &self.insecure_registry)
            .field("fetch_timeout", &self.fetch_timeout)
            .field("authenticated", &self.credentials.is_some())
            .field("extra_ca_bundles", &self.ca_bundles.len())
            .finish()
    }
}

/// Contextual refusal from the extra OCI trust-root boundary.
#[derive(Debug)]
pub struct ComponentArtifactCaError {
    path: PathBuf,
    source: std::io::Error,
}

impl ComponentArtifactCaError {
    fn unreadable(path: &Path, source: std::io::Error) -> Self {
        Self {
            path: path.to_owned(),
            source,
        }
    }
}

impl fmt::Display for ComponentArtifactCaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "OCI CA bundle {}: oci-ca-bundle-unreadable",
            self.path.display()
        )
    }
}

impl std::error::Error for ComponentArtifactCaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Stable classification of a refused component artifact pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentArtifactFetchErrorKind {
    /// The supplied admitted digest cannot name an immutable artifact.
    InvalidReference,
    /// The registry or named artifact is not currently available.
    Unavailable,
    /// The registry answered with bytes or metadata that contradict admission.
    Mismatched,
}

/// Contextual refusal from the component artifact transfer boundary.
pub struct ComponentArtifactFetchError {
    kind: ComponentArtifactFetchErrorKind,
    reference: Option<Box<str>>,
    refusal: &'static str,
}

impl ComponentArtifactFetchError {
    /// Stable refusal class for callers that must not match display text.
    pub fn kind(&self) -> ComponentArtifactFetchErrorKind {
        self.kind
    }

    /// Stable literal naming the exact refused invariant or transfer phase.
    pub fn refusal(&self) -> &'static str {
        self.refusal
    }

    fn invalid_reference() -> Self {
        Self {
            kind: ComponentArtifactFetchErrorKind::InvalidReference,
            reference: None,
            refusal: "component-artifact-digest-invalid",
        }
    }

    fn mismatched(reference: &str, refusal: &'static str) -> Self {
        Self {
            kind: ComponentArtifactFetchErrorKind::Mismatched,
            reference: Some(reference.into()),
            refusal,
        }
    }

    fn from_transport(
        reference: &str,
        unavailable: &'static str,
        mismatched: &'static str,
        source: OciDistributionError,
    ) -> Self {
        let (kind, refusal) = if transport_is_mismatched(&source) {
            (ComponentArtifactFetchErrorKind::Mismatched, mismatched)
        } else {
            (ComponentArtifactFetchErrorKind::Unavailable, unavailable)
        };
        Self {
            kind,
            reference: Some(reference.into()),
            refusal,
        }
    }
}

impl fmt::Debug for ComponentArtifactFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The upstream registry error is deliberately omitted: this type is
        // safe to log without emitting response bodies or future auth context.
        formatter
            .debug_struct("ComponentArtifactFetchError")
            .field("kind", &self.kind)
            .field("reference", &self.reference)
            .field("refusal", &self.refusal)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ComponentArtifactFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(reference) = &self.reference {
            write!(
                formatter,
                "component artifact {reference}: {}",
                self.refusal
            )
        } else {
            write!(formatter, "component artifact: {}", self.refusal)
        }
    }
}

impl std::error::Error for ComponentArtifactFetchError {}

/// Anonymous OCI source that returns only fully verified component bytes.
#[derive(Clone)]
pub struct ComponentArtifactSource {
    client: OciClient,
    base: ComponentArtifactBase,
    auth: RegistryAuth,
}

impl ComponentArtifactSource {
    /// Construct a source from explicit validated transport configuration.
    pub fn new(config: ComponentArtifactSourceConfig) -> Self {
        let protocol = if config.insecure_registry {
            ClientProtocol::HttpsExcept(vec![config.base.registry().to_owned()])
        } else {
            ClientProtocol::Https
        };
        let client = OciClient::new(ClientConfig {
            protocol,
            read_timeout: Some(config.fetch_timeout),
            connect_timeout: Some(config.fetch_timeout),
            extra_root_certificates: config
                .ca_bundles
                .into_iter()
                .map(|data| Certificate {
                    encoding: CertificateEncoding::Pem,
                    data,
                })
                .collect(),
            ..ClientConfig::default()
        });
        Self {
            client,
            base: config.base,
            auth: config
                .credentials
                .map_or(RegistryAuth::Anonymous, |credentials| {
                    RegistryAuth::Basic(
                        credentials.username().to_owned(),
                        credentials.password().to_owned(),
                    )
                }),
        }
    }

    /// Pull and verify the exact bytes named by one admitted component fact.
    pub async fn pull_verified(
        &self,
        component: &AdmittedComponent,
    ) -> Result<Vec<u8>, ComponentArtifactFetchError> {
        let artifact = self
            .base
            .reference(&component.component_digest)
            .map_err(|_| ComponentArtifactFetchError::invalid_reference())?;
        let reference = Reference::with_tag(
            artifact.registry().to_owned(),
            artifact.repository().to_owned(),
            artifact.tag().to_owned(),
        );
        let named = artifact.to_string();
        let expected_config = component_artifact_config_bytes(component);
        let expected_config_digest = component_digest(&expected_config);

        let (manifest, _) = self
            .client
            .pull_image_manifest(&reference, &self.auth)
            .await
            .map_err(|source| {
                ComponentArtifactFetchError::from_transport(
                    &named,
                    "component-artifact-manifest-unavailable",
                    "component-artifact-manifest-invalid",
                    source,
                )
            })?;
        let descriptors = verify_manifest(
            &manifest,
            &component.component_digest,
            &expected_config_digest,
            expected_config.len(),
            &named,
        )?;

        let mut component_bytes = Vec::new();
        self.client
            .pull_blob(&reference, descriptors.component, &mut component_bytes)
            .await
            .map_err(|source| {
                ComponentArtifactFetchError::from_transport(
                    &named,
                    "component-artifact-body-unavailable",
                    "component-artifact-body-transfer-mismatch",
                    source,
                )
            })?;
        verify_component_body(
            &component_bytes,
            descriptors.component.size,
            &component.component_digest,
            &named,
        )?;

        let mut config_bytes = Vec::new();
        self.client
            .pull_blob(&reference, descriptors.config, &mut config_bytes)
            .await
            .map_err(|source| {
                ComponentArtifactFetchError::from_transport(
                    &named,
                    "component-artifact-config-unavailable",
                    "component-artifact-config-transfer-mismatch",
                    source,
                )
            })?;
        verify_config_body(
            &config_bytes,
            descriptors.config.size,
            &expected_config_digest,
            &expected_config,
            &named,
        )?;

        Ok(component_bytes)
    }
}

impl fmt::Debug for ComponentArtifactSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComponentArtifactSource")
            .field("registry", &self.base.registry())
            .field("repository", &self.base.repository())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct VerifiedManifest<'a> {
    component: &'a OciDescriptor,
    config: &'a OciDescriptor,
}

fn verify_manifest<'a>(
    manifest: &'a OciImageManifest,
    expected_component_digest: &str,
    expected_config_digest: &str,
    expected_config_size: usize,
    reference: &str,
) -> Result<VerifiedManifest<'a>, ComponentArtifactFetchError> {
    let refuse = |literal| ComponentArtifactFetchError::mismatched(reference, literal);
    let expected_layout = component_artifact_layout(&[], &[]);
    if manifest.schema_version != expected_layout.manifest_schema_version() {
        return Err(refuse("component-artifact-manifest-schema-mismatch"));
    }
    if manifest.layers.len() != expected_layout.layer_count() {
        return Err(refuse("component-artifact-layer-cardinality-mismatch"));
    }
    let component = &manifest.layers[0];
    if component.media_type != expected_layout.layer_media_type() {
        return Err(refuse("component-artifact-layer-media-type-mismatch"));
    }
    if component.digest != expected_component_digest {
        return Err(refuse("component-artifact-layer-digest-mismatch"));
    }
    if component.size < 0 {
        return Err(refuse("component-artifact-layer-size-mismatch"));
    }

    let config = &manifest.config;
    if config.media_type != expected_layout.config_media_type() {
        return Err(refuse("component-artifact-config-media-type-mismatch"));
    }
    if config.digest != expected_config_digest {
        return Err(refuse("component-artifact-config-digest-mismatch"));
    }
    if config.size != i64::try_from(expected_config_size).unwrap_or(i64::MAX) {
        return Err(refuse("component-artifact-config-size-mismatch"));
    }
    Ok(VerifiedManifest { component, config })
}

fn verify_component_body(
    bytes: &[u8],
    descriptor_size: i64,
    expected_digest: &str,
    reference: &str,
) -> Result<(), ComponentArtifactFetchError> {
    if i64::try_from(bytes.len()).unwrap_or(i64::MAX) != descriptor_size {
        return Err(ComponentArtifactFetchError::mismatched(
            reference,
            "component-artifact-body-size-mismatch",
        ));
    }
    if component_digest(bytes) != expected_digest {
        return Err(ComponentArtifactFetchError::mismatched(
            reference,
            "component-artifact-body-digest-mismatch",
        ));
    }
    Ok(())
}

fn verify_config_body(
    bytes: &[u8],
    descriptor_size: i64,
    expected_digest: &str,
    expected_bytes: &[u8],
    reference: &str,
) -> Result<(), ComponentArtifactFetchError> {
    if i64::try_from(bytes.len()).unwrap_or(i64::MAX) != descriptor_size {
        return Err(ComponentArtifactFetchError::mismatched(
            reference,
            "component-artifact-config-body-size-mismatch",
        ));
    }
    if component_digest(bytes) != expected_digest || bytes != expected_bytes {
        return Err(ComponentArtifactFetchError::mismatched(
            reference,
            "component-artifact-config-body-mismatch",
        ));
    }
    Ok(())
}

fn transport_is_mismatched(error: &OciDistributionError) -> bool {
    matches!(
        error,
        OciDistributionError::ConfigConversionError(_)
            | OciDistributionError::DigestError(_)
            | OciDistributionError::ImageIndexParsingNoPlatformResolverError
            | OciDistributionError::IncompatibleLayerMediaTypeError(_)
            | OciDistributionError::JsonError(_)
            | OciDistributionError::ManifestEncodingError(_)
            | OciDistributionError::ManifestParsingError(_)
            | OciDistributionError::PullNoLayersError
            | OciDistributionError::RegistryNoDigestError
            | OciDistributionError::SpecViolationError(_)
            | OciDistributionError::UnsupportedMediaTypeError(_)
            | OciDistributionError::UnsupportedSchemaVersionError(_)
            | OciDistributionError::VersionedParsingError(_)
    )
}

#[cfg(test)]
mod tests {
    use oci_client::errors::DigestError;
    use serde_json::json;
    use wamn_catalog::{
        AdmittedComponent, ComponentCatalogScope, ComponentDeclaration, ComponentPortDeclaration,
        normalize_component_fact,
    };

    use super::*;

    fn admitted(bytes: &[u8]) -> AdmittedComponent {
        normalize_component_fact(
            ComponentDeclaration {
                scope: ComponentCatalogScope {
                    tenant_id: "tenant-a".to_owned(),
                    catalog_id: "orders".to_owned(),
                    catalog_version: 3,
                },
                component: "transform".to_owned(),
                interface_version: "0.1.0".to_owned(),
                operation: "run".to_owned(),
                input_ports: vec![ComponentPortDeclaration {
                    name: "input".to_owned(),
                    schema: json!({}),
                }],
                output_ports: Vec::new(),
                parameters: Vec::new(),
                connections: Vec::new(),
            },
            component_digest(bytes),
            ["wasi:logging/logging@0.1.0".to_owned()],
            Vec::new(),
        )
        .expect("fixture admits")
        .component
    }

    fn descriptor(media_type: &str, digest: &str, size: i64) -> OciDescriptor {
        OciDescriptor {
            media_type: media_type.to_owned(),
            digest: digest.to_owned(),
            size,
            ..OciDescriptor::default()
        }
    }

    fn manifest(component: &AdmittedComponent, component_size: i64) -> OciImageManifest {
        let config = component_artifact_config_bytes(component);
        let layout = component_artifact_layout(&[], &config);
        OciImageManifest {
            schema_version: layout.manifest_schema_version(),
            config: descriptor(
                layout.config_media_type(),
                &component_digest(&config),
                i64::try_from(config.len()).expect("fixture config size fits"),
            ),
            layers: vec![descriptor(
                layout.layer_media_type(),
                &component.component_digest,
                component_size,
            )],
            ..OciImageManifest::default()
        }
    }

    #[test]
    fn source_configuration_is_explicit_and_redacts_rejected_credentials() {
        let config = ComponentArtifactSourceConfig::new(
            "registry.example/wamn/components",
            false,
            Duration::from_secs(9),
        )
        .expect("explicit source validates");
        let rendered = format!("{config:?}");
        assert!(rendered.contains("registry.example"));
        assert!(rendered.contains("9s"));

        let error = ComponentArtifactSourceConfig::new(
            "user:super-secret@registry.example/wamn/components",
            false,
            Duration::from_secs(9),
        )
        .expect_err("embedded credentials refuse");
        assert!(!format!("{error:?} {error}").contains("super-secret"));
    }

    #[test]
    fn manifest_must_match_both_descriptors_and_exactly_one_layer() {
        let component = admitted(b"component-bytes");
        let config = component_artifact_config_bytes(&component);
        let config_digest = component_digest(&config);
        let exact = manifest(&component, 15);
        verify_manifest(
            &exact,
            &component.component_digest,
            &config_digest,
            config.len(),
            "registry.example/wamn/components:tag",
        )
        .expect("exact manifest verifies");

        let cases: [(&str, OciImageManifest); 7] = [
            ("schema", {
                let mut value = exact.clone();
                value.schema_version = 1;
                value
            }),
            ("cardinality", {
                let mut value = exact.clone();
                value.layers.push(value.layers[0].clone());
                value
            }),
            ("layer-media", {
                let mut value = exact.clone();
                value.layers[0].media_type = "application/wasm".to_owned();
                value
            }),
            ("layer-digest", {
                let mut value = exact.clone();
                value.layers[0].digest = component_digest(b"other");
                value
            }),
            ("config-media", {
                let mut value = exact.clone();
                value.config.media_type = "application/json".to_owned();
                value
            }),
            ("config-digest", {
                let mut value = exact.clone();
                value.config.digest = component_digest(b"other");
                value
            }),
            ("config-size", {
                let mut value = exact.clone();
                value.config.size += 1;
                value
            }),
        ];
        for (case, value) in cases {
            let error = verify_manifest(
                &value,
                &component.component_digest,
                &config_digest,
                config.len(),
                "registry.example/wamn/components:tag",
            )
            .expect_err("manifest drift refuses");
            assert_eq!(error.kind(), ComponentArtifactFetchErrorKind::Mismatched);
            assert!(error.refusal().contains(case.split('-').next().unwrap()));
        }
    }

    #[test]
    fn bodies_are_independently_verified_before_component_bytes_return() {
        let bytes = b"component-bytes";
        let component = admitted(bytes);
        verify_component_body(
            bytes,
            i64::try_from(bytes.len()).unwrap(),
            &component.component_digest,
            "reference",
        )
        .expect("exact body verifies");

        let wrong_size = verify_component_body(
            bytes,
            i64::try_from(bytes.len() + 1).unwrap(),
            &component.component_digest,
            "reference",
        )
        .expect_err("descriptor size drift refuses");
        assert_eq!(
            wrong_size.refusal(),
            "component-artifact-body-size-mismatch"
        );

        let wrong_digest =
            verify_component_body(b"other", 5, &component.component_digest, "reference")
                .expect_err("body digest drift refuses");
        assert_eq!(
            wrong_digest.refusal(),
            "component-artifact-body-digest-mismatch"
        );

        let config = component_artifact_config_bytes(&component);
        verify_config_body(
            &config,
            i64::try_from(config.len()).unwrap(),
            &component_digest(&config),
            &config,
            "reference",
        )
        .expect("exact config verifies");
        let wrong_config =
            verify_config_body(b"{}", 2, &component_digest(b"{}"), &config, "reference")
                .expect_err("different config facts refuse");
        assert_eq!(
            wrong_config.refusal(),
            "component-artifact-config-body-mismatch"
        );
    }

    #[test]
    fn transport_integrity_errors_are_not_classified_as_unavailable() {
        let digest_error = OciDistributionError::DigestError(DigestError::VerificationError {
            expected: component_digest(b"expected"),
            actual: component_digest(b"actual"),
        });
        assert!(transport_is_mismatched(&digest_error));
        assert!(!transport_is_mismatched(
            &OciDistributionError::ImageManifestNotFoundError("missing".to_owned())
        ));

        let error = ComponentArtifactFetchError::from_transport(
            "registry.example/wamn/components:tag",
            "component-artifact-manifest-unavailable",
            "component-artifact-manifest-invalid",
            OciDistributionError::ServerError {
                code: 500,
                url: "https://user:secret@registry.example/v2/private".to_owned(),
                message: "registry-controlled-response-body".to_owned(),
            },
        );
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("private"));
        assert!(!rendered.contains("registry-controlled-response-body"));
        assert!(std::error::Error::source(&error).is_none());
    }

    #[tokio::test]
    async fn invalid_admitted_digest_refuses_before_network_io_without_echoing_it() {
        let config = ComponentArtifactSourceConfig::new(
            "localhost:9/wamn/components",
            true,
            Duration::from_millis(1),
        )
        .expect("source config validates");
        let source = ComponentArtifactSource::new(config);
        let mut component = admitted(b"component-bytes");
        component.component_digest = "not-a-digest-containing-private-context".to_owned();

        let error = source
            .pull_verified(&component)
            .await
            .expect_err("invalid admitted digest refuses locally");
        assert_eq!(
            error.kind(),
            ComponentArtifactFetchErrorKind::InvalidReference
        );
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("private-context"));
    }
}

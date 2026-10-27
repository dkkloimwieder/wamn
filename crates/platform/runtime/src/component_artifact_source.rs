//! Digest-verified OCI supply of admitted component bytes.
//!
//! This module owns only artifact transfer. Callers provide a complete admitted
//! component fact and explicit registry configuration; the source derives the
//! immutable reference, verifies the manifest and both blobs, and returns only
//! component bytes checked against that fact. It owns no catalog read, cache,
//! instance pool, or router behavior. The source implements
//! [`ArtifactSource`] from `wamn-engine`, which holds the digest checks and the
//! local file source.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol};
use oci_client::errors::OciDistributionError;
use oci_client::manifest::{OciDescriptor, OciImageManifest};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use wamn_catalog::{
    AdmittedComponent, COMPOSED_ENTRY_REPOSITORY, ComponentArtifactBase,
    ComponentArtifactReferenceError, parse_component_artifact_base, validate_composed_entry_path,
};
use wamn_engine::artifact_source::{
    ArtifactSource, ComponentArtifactFetchError, VerifiedEntry, verify_component_body,
    verify_config_body,
};
use wamn_engine::component_artifact::{component_artifact_config_bytes, component_artifact_layout};
use wamn_package_artifact::registry_credentials::{
    RegistryCredentialSource, RegistryCredentials, RegistryCredentialsError,
    read_registry_credentials,
};
use wamn_package_artifact::{ComponentArtifactCaError, read_ca_bundles, registry_pull};

use crate::registry_transport::transport_is_mismatched;

/// Explicit, validated configuration for one component artifact repository.
#[derive(Clone, PartialEq, Eq)]
pub struct ComponentArtifactSourceConfig {
    base: ComponentArtifactBase,
    insecure_registry: bool,
    fetch_timeout: Duration,
    credentials: Option<RegistryCredentialSource>,
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
    /// This duplication is measured and recorded, not an oversight: vanilla
    /// wash-runtime keeps the matching reader `oci::extra_ca_certificates()`
    /// private and re-exports no `oci_client` symbol, so installed trust cannot
    /// be read back into a foreign `ClientConfig`. Its only public transfer
    /// surface, `oci::{pull_component, push_component}`, is fixed to
    /// `WASM_LAYER_MEDIA_TYPE` + `WasmConfig` and cannot carry this artifact's
    /// platform-owned layer and config blob — the config blob being the very
    /// admission fact [`ArtifactSource::pull_verified`] checks again. See
    /// `docs/architecture/native-alignment.md#retained-wamn-implementations`
    /// (`wamn-kdhw`) for the exit condition; do not "fix" this by routing the
    /// pull through `pull_component`.
    ///
    /// Pass the same paths the host passed to that call, and call it first:
    /// validation lives there, and it refuses a bundle that is unreadable or
    /// unusable as a trust root rather than starting a host that will reject
    /// every pull. That refusal names the exact unreadable path.
    /// [`ComponentArtifactSource::new`] catches the rest: a bundle that reads
    /// but does not parse is refused there, without a name. It is refused
    /// rather than tolerated because `oci-client`'s `Client::new` answers a
    /// rejected configuration with a wholly default client, discarding the
    /// registry protocol and the timeouts along with the trust roots and
    /// leaving only a warning to say so.
    ///
    /// Empty `paths` leave this source on the compiled-in roots.
    pub fn with_ca_paths(mut self, paths: &[PathBuf]) -> Result<Self, ComponentArtifactCaError> {
        self.ca_bundles.extend(read_ca_bundles(paths)?);
        Ok(self)
    }

    /// Authenticate pulls with one complete credential for this exact registry.
    pub fn with_credentials(mut self, credentials: RegistryCredentials) -> Self {
        self.credentials = Some(RegistryCredentialSource::Fixed(credentials));
        self
    }

    /// Authenticate each pull with a token from the metadata server, asked
    /// for at that pull.
    pub fn with_registry_token_metadata(mut self) -> Self {
        self.credentials = Some(RegistryCredentialSource::MetadataServer);
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

/// Environment variable carrying `--oci-ca-path` values, comma-delimited.
///
/// The spelling wash's own host CLI uses (`crates/wash/src/cli/host.rs`).
pub const OCI_CA_PATHS_ENV: &str = "WASH_OCI_CA_PATHS";

/// Classify a registry transport failure as a mismatch or as absence.
fn transport_error(
    reference: &str,
    unavailable: &'static str,
    mismatched: &'static str,
    source: &OciDistributionError,
) -> ComponentArtifactFetchError {
    if transport_is_mismatched(source) {
        ComponentArtifactFetchError::mismatched(reference, mismatched)
    } else {
        ComponentArtifactFetchError::unavailable(reference, unavailable)
    }
}

/// OCI registry source that returns only verified component bytes.
///
/// Each pull builds its own `oci-client`. A client keeps the first credential
/// it is given for a registry, so a client kept across pulls would keep an
/// expired metadata token.
#[derive(Clone)]
pub struct ComponentArtifactSource {
    config: ComponentArtifactSourceConfig,
}

impl ComponentArtifactSource {
    /// Construct a source from explicit validated transport configuration.
    ///
    /// The client is built once here, so an unusable configuration refuses
    /// before the first pull.
    pub fn new(config: ComponentArtifactSourceConfig) -> Result<Self, ComponentArtifactFetchError> {
        registry_client(&config)?;
        Ok(Self { config })
    }
}

/// One registry client for this source's configuration.
///
/// Built through `TryFrom`, not `Client::new`: that constructor answers a
/// rejected configuration with a warning and a wholly default client, which
/// drops the trust roots, the protocol and the timeouts and turns an unusable
/// CA bundle into a confusing TLS failure on the first pull.
fn registry_client(
    config: &ComponentArtifactSourceConfig,
) -> Result<OciClient, ComponentArtifactFetchError> {
    let protocol = if config.insecure_registry {
        ClientProtocol::HttpsExcept(vec![config.base.registry().to_owned()])
    } else {
        ClientProtocol::Https
    };
    OciClient::try_from(ClientConfig {
        protocol,
        read_timeout: Some(config.fetch_timeout),
        connect_timeout: Some(config.fetch_timeout),
        extra_root_certificates: config
            .ca_bundles
            .iter()
            .map(|data| Certificate {
                encoding: CertificateEncoding::Pem,
                data: data.clone(),
            })
            .collect(),
        ..ClientConfig::default()
    })
    .map_err(|_| ComponentArtifactFetchError::registry_client())
}

#[async_trait]
impl ArtifactSource for ComponentArtifactSource {
    async fn pull_verified(
        &self,
        component: &AdmittedComponent,
    ) -> Result<Vec<u8>, ComponentArtifactFetchError> {
        let deadline = tokio::time::Instant::now() + self.config.fetch_timeout;
        let artifact = self
            .config
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

        let auth = self.auth(artifact.registry(), &named).await?;
        let client = registry_client(&self.config)?;

        let (manifest, _) = registry_pull::retry(
            &reference,
            deadline,
            || client.pull_image_manifest(&reference, &auth),
            |error| error,
        )
        .await
        .map_err(|source| {
            transport_error(
                &named,
                "component-artifact-manifest-unavailable",
                "component-artifact-manifest-invalid",
                &source,
            )
        })?;
        let descriptors = verify_manifest(
            &manifest,
            &component.component_digest,
            expected_config.len(),
            &named,
        )?;

        let component_bytes =
            registry_pull::blob(&client, &reference, descriptors.component, deadline)
                .await
                .map_err(|source| {
                    transport_error(
                        &named,
                        "component-artifact-body-unavailable",
                        "component-artifact-body-transfer-mismatch",
                        &source,
                    )
                })?;
        verify_component_body(
            &component_bytes,
            descriptors.component.size,
            &component.component_digest,
            &named,
        )?;

        let config_bytes = registry_pull::blob(&client, &reference, descriptors.config, deadline)
            .await
            .map_err(|source| {
                transport_error(
                    &named,
                    "component-artifact-config-unavailable",
                    "component-artifact-config-transfer-mismatch",
                    &source,
                )
            })?;
        verify_config_body(
            &config_bytes,
            descriptors.config.size,
            &expected_config,
            &named,
        )?;

        Ok(component_bytes)
    }

    /// Pull the entry beside the members, under the component artifact base,
    /// by the manifest digest that the release records.
    async fn pull_entry(
        &self,
        reference: &str,
    ) -> Result<VerifiedEntry, ComponentArtifactFetchError> {
        let deadline = tokio::time::Instant::now() + self.config.fetch_timeout;
        validate_composed_entry_path(reference)
            .map_err(|_| ComponentArtifactFetchError::invalid_reference())?;
        let digest = reference
            .strip_prefix(COMPOSED_ENTRY_REPOSITORY)
            .and_then(|rest| rest.strip_prefix('@'))
            .expect("a valid entry path names its manifest digest");
        let registry = self.config.base.registry();
        let entry = Reference::with_digest(
            registry.to_owned(),
            format!(
                "{}/{COMPOSED_ENTRY_REPOSITORY}",
                self.config.base.repository()
            ),
            digest.to_owned(),
        );
        let named = entry.whole();
        let auth = self.auth(registry, &named).await?;
        let client = registry_client(&self.config)?;
        // The client checks the manifest against the digest in the reference.
        let (manifest, _) = registry_pull::retry(
            &entry,
            deadline,
            || client.pull_image_manifest(&entry, &auth),
            |error| error,
        )
        .await
        .map_err(|source| {
            transport_error(
                &named,
                "composed-entry-manifest-unavailable",
                "composed-entry-manifest-invalid",
                &source,
            )
        })?;
        let [layer] = manifest.layers.as_slice() else {
            return Err(ComponentArtifactFetchError::mismatched(
                &named,
                "composed-entry-layer-cardinality-mismatch",
            ));
        };
        if layer.media_type != COMPOSED_ENTRY_LAYER_MEDIA_TYPE {
            return Err(ComponentArtifactFetchError::mismatched(
                &named,
                "composed-entry-layer-media-type-mismatch",
            ));
        }
        let bytes = registry_pull::blob(&client, &entry, layer, deadline)
            .await
            .map_err(|source| {
                transport_error(
                    &named,
                    "composed-entry-body-unavailable",
                    "composed-entry-body-transfer-mismatch",
                    &source,
                )
            })?;
        verify_component_body(&bytes, layer.size, &layer.digest, &named)?;
        Ok(VerifiedEntry {
            bytes,
            digest: digest.to_owned(),
        })
    }
}

/// The layer media type that publication pushes the composed entry with.
const COMPOSED_ENTRY_LAYER_MEDIA_TYPE: &str = "application/wasm";

impl ComponentArtifactSource {
    /// The pull credential of `registry`, read fresh for each pull.
    async fn auth(
        &self,
        registry: &str,
        named: &str,
    ) -> Result<RegistryAuth, ComponentArtifactFetchError> {
        let Some(source) = &self.config.credentials else {
            return Ok(RegistryAuth::Anonymous);
        };
        let credentials = source
            .credentials(registry)
            .await
            .map_err(|error| ComponentArtifactFetchError::unavailable(named, error.refusal()))?;
        Ok(RegistryAuth::Basic(
            credentials.username().to_owned(),
            credentials.password().to_owned(),
        ))
    }
}

impl fmt::Debug for ComponentArtifactSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComponentArtifactSource")
            .field("registry", &self.config.base.registry())
            .field("repository", &self.config.base.repository())
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
    if config.size != i64::try_from(expected_config_size).unwrap_or(i64::MAX) {
        return Err(refuse("component-artifact-config-size-mismatch"));
    }
    Ok(VerifiedManifest { component, config })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use wamn_engine::component_admission::component_digest;

    use oci_client::errors::DigestError;
    use serde_json::json;
    use wamn_catalog::{
        ComponentDeclaration, ComponentOperationDeclaration, ComponentPackageScope,
        ComponentPortDeclaration, normalize_component_fact,
    };

    use super::*;
    use wamn_engine::artifact_source::ComponentArtifactFetchErrorType;

    fn admitted(bytes: &[u8]) -> AdmittedComponent {
        normalize_component_fact(
            ComponentDeclaration {
                scope: ComponentPackageScope {
                    tenant_id: "tenant-a".to_owned(),
                    package_id: "orders".to_owned(),
                    package_version: "1.0.0".to_owned(),
                },
                component: "transform".to_owned(),
                interface_version: "0.1.0".to_owned(),
                operations: BTreeMap::from([(
                    "run".to_owned(),
                    ComponentOperationDeclaration {
                        committed_result_schema: None,
                        registered_operation: None,
                        input_ports: vec![ComponentPortDeclaration {
                            name: "input".to_owned(),
                            schema: json!({}),
                        }],
                        output_ports: Vec::new(),
                        parameters: Vec::new(),
                    },
                )]),
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

    #[tokio::test]
    async fn registry_pull_retries_transient_answers_only() {
        let body = b"component-bytes";
        let component = admitted(body);
        let config = component_artifact_config_bytes(&component);
        let manifest =
            serde_json::to_vec(&manifest(&component, body.len().try_into().unwrap())).unwrap();
        for (status, attempts) in [(503, 2), (404, 1), (504, 4), (401, 1), (403, 1), (429, 2)] {
            let mut responses = vec![(status, Vec::new())];
            if attempts == 2 {
                responses.extend([
                    (200, manifest.clone()),
                    (200, body.to_vec()),
                    (200, config.clone()),
                ]);
            }
            let server = registry_pull::test_server::Server::start(responses).await;
            let source = ComponentArtifactSource::new(
                ComponentArtifactSourceConfig::new(
                    &format!("{}/components", server.reference.registry()),
                    true,
                    Duration::from_secs(30),
                )
                .unwrap(),
            )
            .unwrap();
            let result = source.pull_verified(&component).await;
            if attempts == 2 {
                assert_eq!(result.unwrap(), body);
                assert_eq!(server.requests(), 4);
            } else {
                assert!(result.is_err());
                assert_eq!(server.requests(), attempts);
            }
        }
    }

    #[tokio::test]
    async fn registry_pull_does_not_retry_manifest_decode_errors() {
        let server =
            registry_pull::test_server::Server::start(vec![(200, b"not json".to_vec())]).await;
        let source = ComponentArtifactSource::new(
            ComponentArtifactSourceConfig::new(
                &format!("{}/components", server.reference.registry()),
                true,
                Duration::from_secs(3),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(source.pull_verified(&admitted(b"component")).await.is_err());
        assert_eq!(server.requests(), 1);
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
    fn ca_bundles_read_in_order_and_an_unreadable_path_refuses_by_name() {
        let root = std::env::temp_dir().join(format!("wamn-oci-ca-bundles-{}", std::process::id()));
        std::fs::create_dir(&root).expect("create owned CA bundle fixture");
        let first = root.join("first.pem");
        let second = root.join("second.pem");
        std::fs::write(&first, b"first bundle").unwrap();
        std::fs::write(&second, b"second bundle").unwrap();

        assert_eq!(
            read_ca_bundles(&[second.clone(), first.clone()]).unwrap(),
            vec![b"second bundle".to_vec(), b"first bundle".to_vec()]
        );
        assert!(read_ca_bundles(&[]).unwrap().is_empty());

        let missing = root.join("missing.pem");
        let error = read_ca_bundles(&[first.clone(), missing.clone()])
            .expect_err("an unreadable bundle refuses");
        assert!(error.to_string().contains(&missing.display().to_string()));
        assert!(std::error::Error::source(&error).is_some());

        std::fs::remove_file(first).unwrap();
        std::fs::remove_file(second).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn manifest_must_match_both_descriptors_and_exactly_one_layer() {
        let component = admitted(b"component-bytes");
        let config = component_artifact_config_bytes(&component);
        let exact = manifest(&component, 15);
        verify_manifest(
            &exact,
            &component.component_digest,
            config.len(),
            "registry.example/wamn/components:tag",
        )
        .expect("exact manifest verifies");

        let cases: [(&str, OciImageManifest); 6] = [
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
                config.len(),
                "registry.example/wamn/components:tag",
            )
            .expect_err("manifest drift refuses");
            assert_eq!(
                error.error_type(),
                ComponentArtifactFetchErrorType::Mismatched
            );
            assert!(error.refusal().contains(case.split('-').next().unwrap()));
        }
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

        let error = transport_error(
            "registry.example/wamn/components:tag",
            "component-artifact-manifest-unavailable",
            "component-artifact-manifest-invalid",
            &OciDistributionError::ServerError {
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
        let source = ComponentArtifactSource::new(config).expect("registry client builds");
        let mut component = admitted(b"component-bytes");
        component.component_digest = "not-a-digest-containing-private-context".to_owned();

        let error = source
            .pull_verified(&component)
            .await
            .expect_err("invalid admitted digest refuses locally");
        assert_eq!(
            error.error_type(),
            ComponentArtifactFetchErrorType::InvalidReference
        );
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("private-context"));
    }
}

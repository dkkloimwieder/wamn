//! Digest-verified OCI supply of admitted component bytes.
//!
//! This module owns only artifact transfer. Callers provide a complete admitted
//! component fact and explicit registry configuration; the source derives the
//! immutable reference, verifies the manifest and both blobs, and returns only
//! component bytes checked against that fact. It owns no catalog read, cache,
//! instance pool, router behavior, or retry policy. The source implements
//! [`ArtifactSource`] from `wamn-engine`, which holds the digest checks and the
//! local file source.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol};
use oci_client::errors::OciDistributionError;
use oci_client::manifest::{OciDescriptor, OciImageManifest};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use wamn_catalog::AdmittedComponent;

use crate::registry_credentials::{
    RegistryCredentials, RegistryCredentialsError, read_registry_credentials,
};
use crate::registry_transport::transport_is_mismatched;
use wamn_engine::artifact_source::{
    ArtifactSource, ComponentArtifactFetchError, verify_component_body, verify_config_body,
};
use wamn_engine::component_admission::component_digest;
use wamn_engine::component_artifact::{
    ComponentArtifactBase, ComponentArtifactReferenceError, component_artifact_config_bytes,
    component_artifact_layout, parse_component_artifact_base,
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

/// Environment variable carrying `--oci-ca-path` values, comma-delimited.
///
/// The spelling wash's own host CLI uses (`crates/wash/src/cli/host.rs`).
pub const OCI_CA_PATHS_ENV: &str = "WASH_OCI_CA_PATHS";

/// Read the PEM CA bundles at `paths`, in order, as extra OCI trust roots.
///
/// The one reader behind [`ComponentArtifactSourceConfig::with_ca_paths`] and
/// the ctl publishers, which build their own `oci-client` for the same
/// registry. It reads the files and does not check that each bundle is a usable
/// trust root; the builder above says where that check lives and what
/// `oci-client` does without it.
pub fn read_ca_bundles(paths: &[PathBuf]) -> Result<Vec<Vec<u8>>, ComponentArtifactCaError> {
    paths
        .iter()
        .map(|path| {
            std::fs::read(path).map_err(|source| ComponentArtifactCaError::unreadable(path, source))
        })
        .collect()
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
#[derive(Clone)]
pub struct ComponentArtifactSource {
    client: OciClient,
    base: ComponentArtifactBase,
    auth: RegistryAuth,
}

impl ComponentArtifactSource {
    /// Construct a source from explicit validated transport configuration.
    ///
    /// Built through `TryFrom`, not `Client::new`: that constructor answers a
    /// rejected configuration with a warning and a wholly default client, which
    /// drops the trust roots, the protocol and the timeouts and turns an
    /// unusable CA bundle into a confusing TLS failure on the first pull.
    pub fn new(config: ComponentArtifactSourceConfig) -> Result<Self, ComponentArtifactFetchError> {
        let protocol = if config.insecure_registry {
            ClientProtocol::HttpsExcept(vec![config.base.registry().to_owned()])
        } else {
            ClientProtocol::Https
        };
        let client = OciClient::try_from(ClientConfig {
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
        })
        .map_err(|_| ComponentArtifactFetchError::registry_client())?;
        Ok(Self {
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
        })
    }
}

#[async_trait]
impl ArtifactSource for ComponentArtifactSource {
    async fn pull_verified(
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
            &expected_config_digest,
            expected_config.len(),
            &named,
        )?;

        let mut component_bytes = Vec::new();
        self.client
            .pull_blob(&reference, descriptors.component, &mut component_bytes)
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

        let mut config_bytes = Vec::new();
        self.client
            .pull_blob(&reference, descriptors.config, &mut config_bytes)
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use oci_client::errors::DigestError;
    use serde_json::json;
    use wamn_catalog::{
        ComponentDeclaration, ComponentOperationDeclaration, ComponentPackageScope,
        ComponentPortDeclaration, normalize_component_fact,
    };

    use super::*;
    use wamn_engine::artifact_source::ComponentArtifactFetchErrorKind;

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
                        pre_commit: None,
                        committed_result_schema: None,
                        fresh_only: false,
                        registered_operation: None,
                        dependencies: Vec::new(),
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
            error.kind(),
            ComponentArtifactFetchErrorKind::InvalidReference
        );
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("private-context"));
    }
}

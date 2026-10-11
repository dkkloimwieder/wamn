//! Read package artifacts (docs/plan/platform-deploy.md §7.2, R2, R11).
//!
//! A package artifact is the one owner of the component facts of its package,
//! and the OCI registry is the one owner of package facts: no database holds a
//! package row. `push-package` pushes an artifact only after every check
//! passed, so its existence is the proof. This crate pulls an artifact by its
//! full digest or by its tag `<package_id>-<version>`, checks the manifest and
//! every layer against their digests, and reads the component descriptor
//! layers. Publish, wiring authorship and the management gate read component
//! facts through it, so they read them the same way.
//!
//! `wamn-control` packs and pushes package artifacts with [`Repository`]. A
//! deployed service may depend on this crate, never on `wamn-control`.
//!
//! The crate also owns the registry credential ([`registry_credentials`]) and
//! the OCI trust roots ([`read_ca_bundles`]) that every registry client reads,
//! the host's included. It depends on neither `wamn-runtime` nor `wamn-engine`,
//! so it stays small; the artifact reference rule is in `wamn-catalog`.

pub mod registry_credentials;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use oci_client::client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol};
use oci_client::errors::{OciDistributionError, OciErrorCode};
use oci_client::manifest::{OCI_IMAGE_MEDIA_TYPE, OciDescriptor, OciImageManifest};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use wamn_catalog::{
    AdmittedComponent, ComponentDescriptor, ComponentPackageScope, parse_component_artifact_base,
};

use crate::registry_credentials::{RegistryCredentials, read_registry_credentials};

/// Artifact type of a package artifact.
pub const PACKAGE_ARTIFACT_TYPE: &str = "application/vnd.wamn.package.v2";

/// Media type of a component descriptor layer.
pub const COMPONENT_DESCRIPTOR_MEDIA_TYPE: &str =
    "application/vnd.wamn.component.descriptor.v1+json";

/// The read and connect bound of one registry request.
const REGISTRY_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Read the PEM CA bundles at `paths`, in order, as extra OCI trust roots.
///
/// The one reader behind the host's component source
/// (`ComponentArtifactSourceConfig::with_ca_paths` in `wamn-runtime`), the
/// package registry client here and the ctl publishers, which build their own
/// `oci-client` for the same registry. It reads the files and does not check that each bundle is a usable
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

/// The registry inputs that read and push package artifacts.
#[derive(Debug, Clone)]
pub struct PackageRegistry {
    /// Explicit `<registry>/<repository>` base for package artifacts.
    pub artifact_base: String,
    /// `.dockerconfigjson` file carrying the registry credential.
    pub registry_auth_file: PathBuf,
    /// Use plain HTTP for exactly the registry in `artifact_base`.
    pub insecure_registry: bool,
    /// PEM CA bundles trusted for the registry.
    pub oci_ca_paths: Vec<PathBuf>,
}

/// The refusal of a package whose tag `<package_id>-<version>` the registry
/// does not hold: it is unknown, or `push-package` never pushed it. Callers
/// find it in the error chain to refuse with a typed answer instead of an
/// internal error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoPackageArtifact {
    pub package_id: String,
    pub version: String,
}

impl std::fmt::Display for NoPackageArtifact {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "package {}@{} has no package artifact in the registry; run push-package",
            self.package_id, self.version
        )
    }
}

impl std::error::Error for NoPackageArtifact {}

impl NoPackageArtifact {
    /// The refusal in the chain of `error`, if the chain holds one.
    pub fn find(error: &anyhow::Error) -> Option<&Self> {
        error.chain().find_map(|cause| cause.downcast_ref::<Self>())
    }
}

/// One layer of a package artifact.
#[derive(Debug, Clone)]
pub struct PackedLayer {
    /// The layer media type.
    pub media_type: String,
    /// The layer annotations.
    pub annotations: BTreeMap<String, String>,
    /// The layer bytes.
    pub bytes: Vec<u8>,
}

/// `sha256:<hex>` of `bytes`.
pub fn sha256_digest(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes))
    )
}

/// Refuse a tag that the OCI distribution grammar does not admit.
///
/// # Errors
///
/// When `tag` is not an OCI tag.
pub fn ensure_tag(tag: &str) -> anyhow::Result<()> {
    let bytes = tag.as_bytes();
    ensure!(
        !bytes.is_empty()
            && bytes.len() <= 128
            && (bytes[0].is_ascii_alphanumeric() || bytes[0] == b'_')
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-')),
        "package tag {tag:?} is not an OCI tag"
    );
    Ok(())
}

/// Refuse a package artifact digest that is not `sha256:` and 64 lowercase
/// hex digits. A prefix is never resolved.
///
/// # Errors
///
/// When `digest` is not a full digest.
pub fn ensure_digest(digest: &str) -> anyhow::Result<()> {
    let hex = digest.strip_prefix("sha256:").unwrap_or_default();
    ensure!(
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')),
        "{digest:?} is not a package artifact digest: give sha256: and all 64 lowercase hex digits, or the tag"
    );
    Ok(())
}

/// Whether `error` says the artifact does not exist.
pub fn artifact_is_absent(error: &OciDistributionError) -> bool {
    match error {
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
        OciDistributionError::ImageManifestNotFoundError(_)
        | OciDistributionError::ServerError { code: 404, .. } => true,
        _ => false,
    }
}

/// No credential: the auth file states anonymous access for this registry.
pub fn registry_auth(credentials: Option<&RegistryCredentials>) -> RegistryAuth {
    credentials.map_or(RegistryAuth::Anonymous, |credentials| {
        RegistryAuth::Basic(
            credentials.username().to_owned(),
            credentials.password().to_owned(),
        )
    })
}

/// One open registry repository and the tag or digest of one package.
pub struct Repository {
    /// The registry client.
    pub client: OciClient,
    /// The credential of the registry.
    pub auth: RegistryAuth,
    /// `<registry>/<repository>:<tag>` or `<registry>/<repository>@<digest>`.
    pub reference: Reference,
}

/// Names the reference only: the credential must not reach a log.
impl std::fmt::Debug for Repository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Repository")
            .field("reference", &self.reference)
            .finish_non_exhaustive()
    }
}

impl Repository {
    /// Open the repository of `registry` at `reference`: a full digest
    /// `sha256:<hex>` or a tag `<package_id>-<version>`.
    ///
    /// # Errors
    ///
    /// When the reference or the base is not valid, or the credential or the
    /// CA bundles cannot be read.
    pub fn open(registry: &PackageRegistry, reference: &str) -> anyhow::Result<Self> {
        let by_digest = reference.starts_with("sha256:");
        if by_digest {
            ensure_digest(reference)?;
        } else {
            ensure_tag(reference)?;
        }
        let base = parse_component_artifact_base(&registry.artifact_base)
            .context("read the package artifact base")?;
        let credentials = read_registry_credentials(&registry.registry_auth_file, base.registry())
            .with_context(|| format!("load the credential for registry {}", base.registry()))?;
        let ca_bundles = read_ca_bundles(&registry.oci_ca_paths)
            .with_context(|| format!("read CA bundles for registry {}", base.registry()))?;
        let protocol = if registry.insecure_registry {
            ClientProtocol::HttpsExcept(vec![base.registry().to_owned()])
        } else {
            ClientProtocol::Https
        };
        // `TryFrom`, not `Client::new`: that constructor answers a rejected
        // configuration with a default client that drops the trust roots.
        let client = OciClient::try_from(ClientConfig {
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
        .with_context(|| format!("build the client for registry {}", base.registry()))?;
        Ok(Self {
            client,
            auth: registry_auth(Some(&credentials)),
            reference: if by_digest {
                Reference::with_digest(
                    base.registry().to_owned(),
                    base.repository().to_owned(),
                    reference.to_owned(),
                )
            } else {
                Reference::with_tag(
                    base.registry().to_owned(),
                    base.repository().to_owned(),
                    reference.to_owned(),
                )
            },
        })
    }

    /// The manifest bytes and digest of the reference, or `None` when it does
    /// not exist. This is the read `push-package` makes of a tag before it
    /// pushes it.
    ///
    /// # Errors
    ///
    /// When the registry cannot be read, or the bytes do not hash to the
    /// digest it reports or to the digest the reference names.
    pub async fn manifest(&self) -> anyhow::Result<Option<(Vec<u8>, String)>> {
        match self
            .client
            .pull_manifest_raw(&self.reference, &self.auth, &[OCI_IMAGE_MEDIA_TYPE])
            .await
        {
            Ok((bytes, digest)) => {
                ensure!(
                    sha256_digest(&bytes) == digest
                        && self.reference.digest().is_none_or(|named| named == digest),
                    "the manifest of package artifact {} does not hash to {digest}",
                    self.reference
                );
                Ok(Some((bytes.to_vec(), digest)))
            }
            Err(error) if artifact_is_absent(&error) => Ok(None),
            Err(error) => {
                Err(error).with_context(|| format!("read package artifact {}", self.reference))
            }
        }
    }

    /// The manifest digest and the layers of the package artifact under the
    /// reference, or `None` when it does not exist. Each layer is checked
    /// against its digest.
    ///
    /// # Errors
    ///
    /// When the reference holds no package artifact, or a layer cannot be
    /// pulled or does not hash to its digest.
    pub async fn pull(&self) -> anyhow::Result<Option<(String, Vec<PackedLayer>)>> {
        let Some((bytes, digest)) = self.manifest().await? else {
            return Ok(None);
        };
        let manifest: OciImageManifest = serde_json::from_slice(&bytes).with_context(|| {
            format!("parse the manifest of package artifact {}", self.reference)
        })?;
        ensure!(
            manifest.artifact_type.as_deref() == Some(PACKAGE_ARTIFACT_TYPE),
            "{} is not a {PACKAGE_ARTIFACT_TYPE} package artifact",
            self.reference
        );
        let mut layers = Vec::new();
        for layer in &manifest.layers {
            layers.push(self.pull_layer(layer).await?);
        }
        Ok(Some((digest, layers)))
    }

    async fn pull_layer(&self, layer: &OciDescriptor) -> anyhow::Result<PackedLayer> {
        let mut bytes = Vec::new();
        self.client
            .pull_blob(&self.reference, layer, &mut bytes)
            .await
            .with_context(|| format!("pull a layer of package artifact {}", self.reference))?;
        ensure!(
            sha256_digest(&bytes) == layer.digest,
            "a layer of package artifact {} does not hash to {}",
            self.reference,
            layer.digest
        );
        Ok(PackedLayer {
            media_type: layer.media_type.clone(),
            annotations: layer
                .annotations
                .clone()
                .unwrap_or_default()
                .into_iter()
                .collect(),
            bytes,
        })
    }
}

/// The manifest digest and the layers of the package artifact under the tag
/// `<package_id>-<version>`.
///
/// # Errors
///
/// [`NoPackageArtifact`] when the registry holds no such tag, or an error when
/// the artifact cannot be pulled.
pub async fn pull_package(
    registry: &PackageRegistry,
    package_id: &str,
    version: &str,
) -> anyhow::Result<(String, Vec<PackedLayer>)> {
    Repository::open(registry, &format!("{package_id}-{version}"))?
        .pull()
        .await?
        .ok_or_else(|| {
            NoPackageArtifact {
                package_id: package_id.to_owned(),
                version: version.to_owned(),
            }
            .into()
        })
}

/// Parse one descriptor layer. Its bytes must be the canonical JSON of the
/// descriptor it holds.
///
/// # Errors
///
/// When the bytes are not a canonical component descriptor.
pub fn parse_descriptor(bytes: &[u8]) -> anyhow::Result<ComponentDescriptor> {
    let descriptor: ComponentDescriptor =
        serde_json::from_slice(bytes).context("parse a component descriptor")?;
    ensure!(
        wamn_execution_contract::canonical_json_bytes(
            &serde_json::to_value(&descriptor).context("encode a component descriptor")?
        ) == bytes,
        "component descriptor {} is not canonical JSON",
        descriptor.component
    );
    Ok(descriptor)
}

/// The component descriptors of the package artifact of
/// `package_id@version`, sorted by component name.
///
/// # Errors
///
/// [`NoPackageArtifact`] when the registry holds no artifact for the package,
/// or an error when the artifact cannot be pulled or a descriptor is not
/// valid.
pub async fn read_descriptors(
    registry: &PackageRegistry,
    package_id: &str,
    version: &str,
) -> anyhow::Result<Vec<ComponentDescriptor>> {
    let (_, layers) = pull_package(registry, package_id, version).await?;
    let mut descriptors = layers
        .iter()
        .filter(|layer| layer.media_type == COMPONENT_DESCRIPTOR_MEDIA_TYPE)
        .map(|layer| parse_descriptor(&layer.bytes))
        .collect::<anyhow::Result<Vec<_>>>()?;
    descriptors.sort_by(|left, right| left.component.cmp(&right.component));
    Ok(descriptors)
}

/// The admitted component facts of one package, for wiring authorship and the
/// management gate (R2): the descriptors of its package artifact, in `scope`.
///
/// # Errors
///
/// As [`read_descriptors`], or when a descriptor carries an invalid
/// effect projection.
pub async fn load_component_facts(
    registry: &PackageRegistry,
    scope: &ComponentPackageScope,
) -> anyhow::Result<Vec<AdmittedComponent>> {
    read_descriptors(registry, &scope.package_id, &scope.package_version)
        .await
        .with_context(|| {
            format!(
                "read the component facts of {}@{}",
                scope.package_id, scope.package_version
            )
        })?
        .into_iter()
        .map(|descriptor| {
            let component = descriptor.into_admitted(scope.clone()).component;
            wamn_catalog::verify_stored_effect_projection(&component).with_context(|| {
                format!(
                    "component {:?} of package {}@{} carries an invalid effect projection",
                    component.component, scope.package_id, scope.package_version
                )
            })?;
            Ok(component)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digest_is_given_in_full() {
        assert!(ensure_digest(&format!("sha256:{}", "a".repeat(64))).is_ok());
        for refused in [
            String::new(),
            "sha256:".to_owned(),
            "sha256:ab12".to_owned(),
            "a".repeat(64),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha256:{}", "a".repeat(65)),
        ] {
            assert!(ensure_digest(&refused).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn a_tag_follows_the_oci_grammar() {
        assert!(ensure_tag("wamn_wms-2.1.0").is_ok());
        assert!(ensure_tag("").is_err());
        assert!(ensure_tag("-leading").is_err());
        assert!(ensure_tag("has/slash").is_err());
    }

    #[test]
    fn the_refusal_is_found_through_context() {
        let error = anyhow::Error::new(NoPackageArtifact {
            package_id: "orders".to_owned(),
            version: "1.0.0".to_owned(),
        })
        .context("open the package");
        let found = NoPackageArtifact::find(&error).expect("the refusal is in the chain");
        assert_eq!(found.package_id, "orders");
        assert!(NoPackageArtifact::find(&anyhow::anyhow!("other")).is_none());
    }

    #[test]
    fn a_descriptor_must_be_canonical() {
        let descriptor = serde_json::json!({
            "component": "entity",
            "interface-version": "0.1",
            "component-digest": format!("sha256:{}", "1".repeat(64)),
            "operations": {},
            "imports": [],
            "imports-fingerprint": format!("sha256:{}", "2".repeat(64)),
            "effects": [],
            "connections": [],
        });
        let canonical = wamn_execution_contract::canonical_json_bytes(&descriptor);
        assert_eq!(
            parse_descriptor(&canonical)
                .expect("canonical bytes parse")
                .component,
            "entity"
        );
        let pretty = serde_json::to_vec_pretty(&descriptor).expect("encode");
        assert!(parse_descriptor(&pretty).is_err());
    }
}

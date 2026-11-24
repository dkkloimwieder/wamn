//! Read package artifacts (docs/operations/building.md#reading-a-pushed-package).
//!
//! A package artifact is the one owner of the component facts of its package,
//! and the OCI registry is the one owner of package facts: no database holds a
//! package row. `push-package` pushes an artifact only after every test
//! passed, so its existence is the proof. This crate pulls an artifact by its
//! full digest or by its tag `<package_id>-<version>`, tests the manifest and
//! every layer against its digest. The first layer contains the published
//! package document, including the admitted component facts.
//!
//! [`crate::release_package`] packs and pushes package artifacts with [`Repository`].
//!
//! The crate also owns the registry credential ([`registry_credentials`]) and
//! the OCI trust roots ([`read_ca_bundles`]) that every registry client reads,
//! the host's included. This module does not use the component adapter or
//! `wamn-engine`; the artifact reference rule is in `wamn-oci-reference`.

pub mod integration;
pub mod registry_credentials;
pub mod registry_pull;
mod registry_source;
pub mod registry_transport;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use oci_client::errors::{OciDistributionError, OciErrorCode};
use oci_client::manifest::{OCI_IMAGE_MEDIA_TYPE, OciDescriptor, OciImageManifest};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use wamn_digest::bytes_sha256;
use wamn_oci_reference::{OciTag, parse_component_artifact_base};

use crate::package_artifact::registry_credentials::{
    RegistryCredentials, read_registry_credentials,
};
use crate::package_artifact::registry_source::RegistrySource;

/// Artifact type of a package artifact.
pub const PACKAGE_ARTIFACT_TYPE: &str = "application/vnd.wamn.package.v3";

/// Standard OCI timestamp used only by development package coordinates.
pub const CREATED_ANNOTATION: &str = "org.opencontainers.image.created";

/// Whether a coordinate ends with the development content digest suffix.
pub fn is_dev_tag(tag: &OciTag) -> bool {
    tag.as_str()
        .rsplit_once("-dev.h")
        .is_some_and(|(prefix, hex)| {
            !prefix.is_empty()
                && hex.len() == 12
                && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// Environment variable carrying the OCI CA paths accepted by wash.
pub const OCI_CA_PATHS_ENV: &str = "WASH_OCI_CA_PATHS";

/// Media type of the published package document, the first layer.
pub const PACKAGE_MANIFEST_MEDIA_TYPE: &str = "application/vnd.wamn.package.manifest.v1+json";

/// Media type of the migrations tar, the second layer.
pub const PACKAGE_TAR_MEDIA_TYPE: &str = "application/vnd.wamn.package.v1.tar";

/// The read and connect bound of one registry request.
const REGISTRY_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Read the PEM CA bundles at `paths`, in order, as extra OCI trust roots.
///
/// The OCI clients share the additional trust roots read from these paths.
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRegistry {
    /// Explicit `<registry>/<repository>` base for package artifacts.
    pub artifact_base: String,
    /// A caller-resolved credential, held only in memory.
    #[serde(skip)]
    pub credentials: Option<RegistryCredentials>,
    /// `.dockerconfigjson` file used when no caller-resolved credential is supplied.
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
    pub package_id: wamn_package::PackageName,
    pub version: wamn_package::package_version::PackageVersion,
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

/// Verified package content and the metadata of its published manifest.
#[derive(Debug)]
pub struct PulledPackage {
    pub digest: wamn_digest::Digest,
    pub layers: Vec<PackedLayer>,
    pub annotations: BTreeMap<String, String>,
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

/// A tag or a parsed immutable artifact identity.
#[derive(Debug, Clone, Copy)]
pub enum ArtifactReference<'a> {
    Tag(&'a OciTag),
    Digest(wamn_digest::Digest),
}

impl<'a> From<&'a OciTag> for ArtifactReference<'a> {
    fn from(tag: &'a OciTag) -> Self {
        Self::Tag(tag)
    }
}

impl From<&wamn_digest::Digest> for ArtifactReference<'_> {
    fn from(digest: &wamn_digest::Digest) -> Self {
        Self::Digest(*digest)
    }
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
    pub fn open<'a>(
        registry: &PackageRegistry,
        reference: impl Into<ArtifactReference<'a>>,
    ) -> anyhow::Result<Self> {
        let base = parse_component_artifact_base(&registry.artifact_base)?;
        let from_file;
        let credentials = if let Some(credentials) = &registry.credentials {
            Some(credentials)
        } else {
            from_file = read_registry_credentials(&registry.registry_auth_file, base.registry())
                .with_context(|| format!("load the credential for registry {}", base.registry()))?;
            from_file.as_ref()
        };
        Self::with_auth(registry, reference, registry_auth(credentials))
    }

    /// Open a repository with a host-resolved credential, including metadata tokens.
    pub fn with_auth<'a>(
        registry: &PackageRegistry,
        reference: impl Into<ArtifactReference<'a>>,
        auth: RegistryAuth,
    ) -> anyhow::Result<Self> {
        let base = parse_component_artifact_base(&registry.artifact_base)
            .context("read the package artifact base")?;
        let ca_bundles = read_ca_bundles(&registry.oci_ca_paths)
            .with_context(|| format!("read CA bundles for registry {}", base.registry()))?;
        let reference = match reference.into() {
            ArtifactReference::Digest(digest) => Reference::with_digest(
                base.registry().to_owned(),
                base.repository().to_owned(),
                digest.to_string(),
            ),
            ArtifactReference::Tag(tag) => Reference::with_tag(
                base.registry().to_owned(),
                base.repository().to_owned(),
                tag.to_string(),
            ),
        };
        let source = RegistrySource::new(base, registry.insecure_registry, REGISTRY_IO_TIMEOUT)
            .with_ca_bundles(ca_bundles);
        let client = source.client().context("build the registry client")?;
        Ok(Self {
            client,
            auth,
            reference,
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
    pub async fn manifest(&self) -> anyhow::Result<Option<(Vec<u8>, wamn_digest::Digest)>> {
        self.manifest_before(tokio::time::Instant::now() + REGISTRY_IO_TIMEOUT)
            .await
    }

    async fn manifest_before(
        &self,
        deadline: tokio::time::Instant,
    ) -> anyhow::Result<Option<(Vec<u8>, wamn_digest::Digest)>> {
        match registry_pull::retry(
            &self.reference,
            deadline,
            || async {
                self.client
                    .pull_manifest_raw(&self.reference, &self.auth, &[OCI_IMAGE_MEDIA_TYPE])
                    .await
            },
            |error| error,
        )
        .await
        {
            Ok((bytes, digest)) => {
                let digest: wamn_digest::Digest = digest.parse()?;
                ensure!(
                    bytes_sha256(&bytes) == digest
                        && self
                            .reference
                            .digest()
                            .is_none_or(|named| named == digest.to_string()),
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

    /// The manifest digest, layers and annotations of the package artifact under the
    /// reference, or `None` when it does not exist. Each layer is validated
    /// against its digest.
    ///
    /// # Errors
    ///
    /// When the reference holds no package artifact, or a layer cannot be
    /// pulled or does not hash to its digest.
    pub async fn pull(&self) -> anyhow::Result<Option<PulledPackage>> {
        let deadline = tokio::time::Instant::now() + REGISTRY_IO_TIMEOUT;
        let Some((bytes, digest)) = self.manifest_before(deadline).await? else {
            return Ok(None);
        };
        let manifest = self.parse_package_image_manifest(&bytes)?;
        let mut layers = Vec::new();
        for layer in &manifest.layers {
            layers.push(self.pull_layer(layer, deadline).await?);
        }
        Ok(Some(PulledPackage {
            digest,
            layers,
            annotations: manifest.annotations.unwrap_or_default(),
        }))
    }

    /// Read the current tag's migration tar only when it differs from the candidate.
    /// The caller already fetched and verified the current tag's image manifest.
    pub async fn predecessor_migrations(
        &self,
        bytes: &[u8],
        candidate: &wamn_digest::Digest,
    ) -> anyhow::Result<(Option<PackedLayer>, Option<String>)> {
        let manifest = self.parse_package_image_manifest(bytes).with_context(|| {
            format!(
                "cannot replace package tag {}; publish under a new tag",
                self.reference
            )
        })?;
        let layer = &manifest.layers[1];
        let migrations = if layer.digest.parse::<wamn_digest::Digest>()? == *candidate {
            None
        } else {
            Some(
                self.pull_layer(layer, tokio::time::Instant::now() + REGISTRY_IO_TIMEOUT)
                    .await?,
            )
        };
        Ok((
            migrations,
            manifest
                .annotations
                .and_then(|mut annotations| annotations.remove(CREATED_ANNOTATION)),
        ))
    }

    fn parse_package_image_manifest(&self, bytes: &[u8]) -> anyhow::Result<OciImageManifest> {
        let manifest: OciImageManifest = serde_json::from_slice(bytes).with_context(|| {
            format!("parse the manifest of package artifact {}", self.reference)
        })?;
        ensure!(
            manifest.artifact_type.as_deref() == Some(PACKAGE_ARTIFACT_TYPE),
            "{} is not a {PACKAGE_ARTIFACT_TYPE} package artifact",
            self.reference
        );
        ensure!(
            matches!(manifest.layers.as_slice(), [package, migrations]
                if package.media_type == PACKAGE_MANIFEST_MEDIA_TYPE
                    && migrations.media_type == PACKAGE_TAR_MEDIA_TYPE),
            "{} must contain wamn.json and a migrations tar, in that order",
            self.reference
        );
        Ok(manifest)
    }

    async fn pull_layer(
        &self,
        layer: &OciDescriptor,
        deadline: tokio::time::Instant,
    ) -> anyhow::Result<PackedLayer> {
        let bytes = registry_pull::blob(&self.client, &self.reference, layer, deadline)
            .await
            .with_context(|| format!("pull a layer of package artifact {}", self.reference))?;
        ensure!(
            bytes_sha256(&bytes) == layer.digest.parse::<wamn_digest::Digest>()?,
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
    package_id: &wamn_package::PackageName,
    version: &wamn_package::package_version::PackageVersion,
) -> anyhow::Result<(wamn_digest::Digest, Vec<PackedLayer>)> {
    Repository::open(
        registry,
        &format!("{package_id}-{version}").parse::<OciTag>()?,
    )?
    .pull()
    .await?
    .map(|package| (package.digest, package.layers))
    .ok_or_else(|| {
        NoPackageArtifact {
            package_id: package_id.to_owned(),
            version: version.clone(),
        }
        .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ca_bundles_read_in_order_and_an_unreadable_path_refuses_by_name() {
        let root = std::env::temp_dir().join(format!("wamn-oci-ca-bundles-{}", std::process::id()));
        std::fs::create_dir(&root).expect("create owned CA bundle fixture");
        let first = root.join("first.pem");
        let second = root.join("second.pem");
        std::fs::write(&first, b"first bundle").unwrap();
        std::fs::write(&second, b"second bundle").unwrap();

        read_ca_bundles(&[second.clone(), first.clone()]).unwrap();
        assert!(read_ca_bundles(&[]).unwrap().is_empty());

        let missing = root.join("missing.pem");
        let error =
            read_ca_bundles(&[first.clone(), missing]).expect_err("an unreadable bundle refuses");
        assert!(std::error::Error::source(&error).is_some());

        std::fs::remove_file(first).unwrap();
        std::fs::remove_file(second).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn the_refusal_is_found_through_context() {
        let error = anyhow::Error::new(NoPackageArtifact {
            package_id: "orders".parse().unwrap(),
            version: "1.0.0".parse().unwrap(),
        })
        .context("open the package");
        let found = NoPackageArtifact::find(&error).expect("the refusal is in the chain");
        assert_eq!(found.package_id, "orders".parse().unwrap());
        assert!(NoPackageArtifact::find(&anyhow::anyhow!("other")).is_none());
    }
}

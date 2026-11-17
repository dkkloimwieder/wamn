//! Stock OCI component reads for publication and workload image selection.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::package_artifact::registry_credentials::{
    RegistryCredentialSource, RegistryCredentials, RegistryCredentialsError,
    read_registry_credentials,
};
use crate::package_artifact::{ComponentArtifactCaError, read_ca_bundles};
use oci_client::Reference;
use wamn_catalog::{ComponentArtifactReferenceError, parse_component_artifact_base};

use crate::package_artifact::registry_source::RegistrySource;

/// Explicit, validated configuration for one component artifact repository.
#[derive(Clone, PartialEq, Eq)]
pub struct ComponentArtifactSourceConfig {
    source: RegistrySource,
}

impl ComponentArtifactSourceConfig {
    /// Validate one explicit `<registry>/<repository>` source configuration.
    pub fn new(
        artifact_base: &str,
        insecure_registry: bool,
        fetch_timeout: Duration,
    ) -> Result<Self, ComponentArtifactReferenceError> {
        let base = parse_component_artifact_base(artifact_base)?;
        let source = RegistrySource::new(base, insecure_registry, fetch_timeout);
        Ok(Self { source })
    }

    /// Trust the PEM CA bundles at `paths` for pulls from this source.
    pub fn with_ca_paths(mut self, paths: &[PathBuf]) -> Result<Self, ComponentArtifactCaError> {
        self.source = self.source.with_ca_bundles(read_ca_bundles(paths)?);
        Ok(self)
    }

    /// Authenticate pulls with one complete credential for this exact registry.
    pub fn with_credentials(mut self, credentials: RegistryCredentials) -> Self {
        self.source = self
            .source
            .with_credentials(RegistryCredentialSource::Fixed(credentials));
        self
    }

    /// Authenticate each pull with a token from the metadata server, asked
    /// for at that pull.
    pub fn with_registry_token_metadata(mut self) -> Self {
        self.source = self
            .source
            .with_credentials(RegistryCredentialSource::MetadataServer);
        self
    }

    /// Load this source's credential from a projected Docker config file.
    pub fn with_registry_auth_file(
        self,
        path: &std::path::Path,
    ) -> Result<Self, RegistryCredentialsError> {
        let credentials = read_registry_credentials(path, self.source.base().registry())?;
        Ok(self.with_credentials(credentials))
    }
}

impl fmt::Debug for ComponentArtifactSourceConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComponentArtifactSourceConfig")
            .field("registry", &self.source.base().registry())
            .field("repository", &self.source.base().repository())
            .field("fetch_timeout", &self.source.timeout())
            .field("source", &self.source)
            .finish()
    }
}

/// Environment variable carrying `--oci-ca-path` values, comma-delimited.
///
/// The spelling wash's own host CLI uses (`crates/wash/src/cli/host.rs`).
pub const OCI_CA_PATHS_ENV: &str = "WASH_OCI_CA_PATHS";

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
    /// Resolve a stock component tag to the immutable image reference used by a workload.
    pub async fn image_reference(&self, digest: &wamn_digest::Digest) -> anyhow::Result<String> {
        let artifact = self.config.source.base().reference(digest);
        let reference = artifact.to_string().parse::<Reference>()?;
        let auth = self.config.source.auth().await?;
        let (manifest, image_digest) = self
            .config
            .source
            .client()?
            .pull_image_manifest(&reference, &auth)
            .await?;
        anyhow::ensure!(
            manifest.layers.len() == 1
                && manifest.layers[0].media_type == "application/wasm"
                && manifest.layers[0].digest == digest.to_string(),
            "component image differs from its package descriptor"
        );
        Ok(format!(
            "{}/{}@{}",
            artifact.registry(),
            artifact.repository(),
            image_digest
        ))
    }

    /// Pull a stock application/wasm artifact for package publication checks.
    pub async fn pull_component(&self, digest: &wamn_digest::Digest) -> anyhow::Result<Vec<u8>> {
        let artifact = self.config.source.base().reference(digest);
        let reference = artifact.to_string().parse::<Reference>()?;
        let auth = self.config.source.auth().await?;
        let mut image = self
            .config
            .source
            .client()?
            .pull(&reference, &auth, vec!["application/wasm"])
            .await?;
        anyhow::ensure!(
            image.layers.len() == 1,
            "component image must contain one layer"
        );
        let bytes = image.layers.remove(0).data;
        anyhow::ensure!(
            wamn_digest::bytes_sha256(&bytes) == *digest,
            "component image differs from its package descriptor"
        );
        Ok(bytes.into())
    }

    /// Construct a source from explicit validated transport configuration.
    ///
    /// The client is built once here, so an unusable configuration refuses
    /// before the first pull.
    pub fn new(config: ComponentArtifactSourceConfig) -> anyhow::Result<Self> {
        config.source.client()?;
        Ok(Self { config })
    }
}

impl fmt::Debug for ComponentArtifactSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComponentArtifactSource")
            .field("registry", &self.config.source.base().registry())
            .field("repository", &self.config.source.base().repository())
            .finish_non_exhaustive()
    }
}

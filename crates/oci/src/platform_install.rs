//! OCI operations for platform and org installation.

use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use wamn_platform_install::{org::RegistryCredentials, release_chart::ChartImageValidation};

use crate::package_artifact::registry_credentials::project_registry_credentials;

/// Resolve and validate OCI inputs for platform installation.
#[derive(Debug)]
pub struct PlatformRegistry;

impl RegistryCredentials for PlatformRegistry {
    fn docker_configuration(&self, registry: &str, pod_registry: &str) -> anyhow::Result<Vec<u8>> {
        let path = std::env::var_os("WAMN_REGISTRY_AUTH_FILE")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .context("registry-credentials-file-required: set WAMN_REGISTRY_AUTH_FILE")?;
        let registry = registry.split('/').next().unwrap_or_default();
        let pod_registry = pod_registry.split('/').next().unwrap_or_default();
        let config = project_registry_credentials(&path, registry, pod_registry)?;
        Ok(serde_json::to_vec(&config)?)
    }
}

impl ChartImageValidation for PlatformRegistry {
    fn require_host_tag(&self, image: &str) -> anyhow::Result<()> {
        let host: oci_client::Reference = image.parse().context("parse the host image")?;
        ensure!(host.tag().is_some(), "the host image requires a tag");
        Ok(())
    }

    fn require_image_digest(&self, image: &str) -> anyhow::Result<()> {
        let reference: oci_client::Reference = image
            .parse()
            .with_context(|| format!("invalid OCI image reference {image}"))?;
        reference
            .digest()
            .context("image requires a SHA-256 digest")?
            .parse::<wamn_digest::Digest>()?;
        Ok(())
    }
}

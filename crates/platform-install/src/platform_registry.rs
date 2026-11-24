//! OCI operations for platform and org installation.

use std::path::PathBuf;

use anyhow::{Context as _, ensure};

use wamn_oci::package_artifact::{
    image_digest, image_has_tag, registry_credentials::project_registry_credentials,
};

pub(crate) fn docker_configuration(registry: &str, pod_registry: &str) -> anyhow::Result<Vec<u8>> {
    let path = std::env::var_os("WAMN_REGISTRY_AUTH_FILE")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .context("registry-credentials-file-required: set WAMN_REGISTRY_AUTH_FILE")?;
    let registry = registry.split('/').next().unwrap_or_default();
    let pod_registry = pod_registry.split('/').next().unwrap_or_default();
    let config = project_registry_credentials(&path, registry, pod_registry)?;
    Ok(serde_json::to_vec(&config)?)
}

pub(crate) fn require_host_tag(image: &str) -> anyhow::Result<()> {
    ensure!(
        image_has_tag(image).context("parse the host image")?,
        "the host image requires a tag"
    );
    Ok(())
}

pub(crate) fn require_image_digest(image: &str) -> anyhow::Result<()> {
    image_digest(image)
        .with_context(|| format!("invalid OCI image reference {image}"))?
        .context("image requires a SHA-256 digest")?
        .parse::<wamn_digest::Digest>()?;
    Ok(())
}

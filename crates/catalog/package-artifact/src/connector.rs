//! Read independently published connector artifacts by their environment pins.

use std::time::Duration;

use anyhow::{Context as _, ensure};
use oci_client::manifest::OciImageManifest;
use serde::Deserialize;

use crate::{PackageRegistry, Repository, ensure_digest, registry_pull, sha256_digest};

/// OCI artifact type for one connector.
pub const ARTIFACT_TYPE: &str = "application/vnd.wamn.connector.v1";
/// OCI configuration media type for connector identity and source bytes.
pub const CONFIG_TYPE: &str = "application/vnd.wamn.connector.config.v1+json";
/// OCI layer media type for connector component bytes.
pub const WASM_TYPE: &str = "application/wasm";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectorConfig {
    code: String,
    form: String,
    source_digest: String,
}

/// Pull exact component bytes for the named integration and full artifact digest.
///
/// # Errors
///
/// Refuses absent artifacts, another integration, and altered manifests or blobs.
pub async fn pull(registry: &PackageRegistry, code: &str, digest: &str) -> anyhow::Result<Vec<u8>> {
    ensure_digest(digest)?;
    let repository = Repository::open(registry, digest)?;
    pull_from(&repository, code, digest).await
}

/// Read a connector through an already authenticated repository.
pub async fn pull_from(
    repository: &Repository,
    code: &str,
    digest: &str,
) -> anyhow::Result<Vec<u8>> {
    ensure_digest(digest)?;
    ensure!(
        repository.reference.digest() == Some(digest),
        "connector repository is not pinned to the requested digest"
    );
    let (bytes, _) = repository
        .manifest()
        .await?
        .context("connector artifact is absent")?;
    let manifest: OciImageManifest = serde_json::from_slice(&bytes)?;
    ensure!(
        manifest.artifact_type.as_deref() == Some(ARTIFACT_TYPE)
            && manifest.config.media_type == CONFIG_TYPE
            && manifest.layers.len() == 1
            && manifest.layers[0].media_type == WASM_TYPE,
        "artifact {digest} is not a wasm connector"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let config = registry_pull::blob(
        &repository.client,
        &repository.reference,
        &manifest.config,
        deadline,
    )
    .await?;
    ensure!(
        sha256_digest(&config) == manifest.config.digest
            && usize::try_from(manifest.config.size) == Ok(config.len()),
        "connector configuration differs from its descriptor"
    );
    let config: ConnectorConfig = serde_json::from_slice(&config)?;
    ensure!(
        config.code == code && config.form == "wasm",
        "connector artifact does not belong to wasm integration {code}"
    );
    ensure_digest(&config.source_digest)?;
    let descriptor = &manifest.layers[0];
    let component = registry_pull::blob(
        &repository.client,
        &repository.reference,
        descriptor,
        deadline,
    )
    .await?;
    ensure!(
        sha256_digest(&component) == descriptor.digest
            && usize::try_from(descriptor.size) == Ok(component.len()),
        "connector component differs from its descriptor"
    );
    Ok(component)
}

//! Read independently published integration artifacts by their environment pins.

use std::time::Duration;

use anyhow::{Context as _, ensure};
use oci_client::manifest::OciImageManifest;
use serde::Deserialize;
use wamn_catalog::RuntimeType;

use crate::{PackageRegistry, Repository, ensure_digest, registry_pull, sha256_digest};

/// OCI artifact type for one integration.
pub const ARTIFACT_TYPE: &str = "application/vnd.wamn.integration.v1";
/// OCI configuration media type for integration identity and source bytes.
pub const CONFIG_TYPE: &str = "application/vnd.wamn.integration.config.v1+json";
/// OCI layer media type for integration component bytes.
pub const WASM_TYPE: &str = "application/wasm";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IntegrationConfig {
    code: String,
    runtime_type: RuntimeType,
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

/// Read an integration through an already authenticated repository.
pub async fn pull_from(
    repository: &Repository,
    code: &str,
    digest: &str,
) -> anyhow::Result<Vec<u8>> {
    ensure_digest(digest)?;
    ensure!(
        repository.reference.digest() == Some(digest),
        "integration repository is not pinned to the requested digest"
    );
    let (bytes, _) = repository
        .manifest()
        .await?
        .context("integration artifact is absent")?;
    let manifest: OciImageManifest = serde_json::from_slice(&bytes)?;
    ensure!(
        manifest.artifact_type.as_deref() == Some(ARTIFACT_TYPE)
            && manifest.config.media_type == CONFIG_TYPE
            && manifest.layers.len() == 1
            && manifest.layers[0].media_type == WASM_TYPE,
        "artifact {digest} is not an integration component"
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
        "integration configuration differs from its descriptor"
    );
    let config: IntegrationConfig = serde_json::from_slice(&config)?;
    ensure!(
        config.code == code
            && matches!(config.runtime_type, RuntimeType::Wasm | RuntimeType::Native),
        "integration artifact does not belong to integration {code}"
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
        "integration component differs from its descriptor"
    );
    Ok(component)
}

#[cfg(test)]
mod tests {
    use super::IntegrationConfig;
    use wamn_catalog::RuntimeType;

    #[test]
    fn artifact_runtime_type_uses_the_current_field_and_closed_values() {
        let mut config = serde_json::json!({
            "code": "modbus",
            "runtime_type": "wasm",
            "source_digest": format!("sha256:{}", "a".repeat(64)),
        });
        let parsed: IntegrationConfig = serde_json::from_value(config.clone()).unwrap();
        assert_eq!(parsed.runtime_type, RuntimeType::Wasm);

        config["runtime_type"] = "component".into();
        assert!(serde_json::from_value::<IntegrationConfig>(config).is_err());
    }
}

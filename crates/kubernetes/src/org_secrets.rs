//! Fill credentials in the org's chart-managed Secrets.

use anyhow::Context as _;
use k8s_openapi::api::core::v1::Secret;
use kube::Api;
use wamn_platform_install::provision_org::{OAUTH_ENCRYPTION_INPUT, ORG_INPUTS_DIR, OrgSecrets};
use wamn_registry::OrgId;

use crate::client::{KubernetesApi, REQUEST_TIMEOUT};

/// Read the OAuth configuration mounted into the SQL Job.
pub async fn oauth_encryption() -> anyhow::Result<serde_json::Value> {
    let path = std::path::Path::new(ORG_INPUTS_DIR).join(OAUTH_ENCRYPTION_INPUT);
    let input = tokio::fs::read(path)
        .await
        .context("read the platform OAuth encryption configuration")?;
    serde_json::from_slice(&input).context("decode the platform OAuth encryption configuration")
}

impl OrgSecrets for KubernetesApi {
    async fn contains_key(&self, org: &OrgId, secret: &str, key: &str) -> anyhow::Result<bool> {
        let secrets = Api::<Secret>::namespaced(self.client(), org.as_str());
        let secret = tokio::time::timeout(REQUEST_TIMEOUT, secrets.get(secret)).await??;
        Ok(secret.data.is_some_and(|data| data.contains_key(key)))
    }

    async fn write_key(
        &self,
        org: &OrgId,
        secret: &str,
        key: &str,
        value: &[u8],
    ) -> anyhow::Result<()> {
        self.patch_namespaced_secret_data(org.as_str(), secret, key, value)
            .await?;
        Ok(())
    }
}

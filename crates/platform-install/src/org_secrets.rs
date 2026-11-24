//! Fill credentials in the org's chart-managed Secrets.

use crate::provision_org::{OAUTH_ENCRYPTION_INPUT, ORG_INPUTS_DIR};
use anyhow::Context as _;

/// Read the OAuth configuration mounted into the SQL Job.
pub async fn oauth_encryption() -> anyhow::Result<serde_json::Value> {
    let path = std::path::Path::new(ORG_INPUTS_DIR).join(OAUTH_ENCRYPTION_INPUT);
    let input = tokio::fs::read(path)
        .await
        .context("read the platform OAuth encryption configuration")?;
    serde_json::from_slice(&input).context("decode the platform OAuth encryption configuration")
}

//! Read connection Secrets in the host's namespace.

use std::{fmt, time::Duration};

use anyhow::{Context as _, ensure};
use k8s_openapi::api::core::v1::Secret;
use kube::{Api, Client, Config};
use tokio::sync::OnceCell;
use wamn_connection::connection_credentials::CredentialStore;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct NoCluster;

impl fmt::Display for NoCluster {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("no credential store in local mode")
    }
}

impl std::error::Error for NoCluster {}

#[derive(Default)]
pub(super) struct KubernetesCredentials {
    secrets: OnceCell<Api<Secret>>,
}

fn in_cluster_secrets() -> anyhow::Result<Api<Secret>> {
    if std::env::var_os("KUBERNETES_SERVICE_HOST").is_none() {
        let error = NoCluster;
        tracing::warn!("{error}");
        return Err(error.into());
    }

    let mut config = Config::incluster().context("load in-cluster credential configuration")?;
    config.default_retry = false;
    let namespace = config.default_namespace.clone();
    let client = Client::try_from(config).context("build in-cluster credential client")?;
    Ok(Api::<Secret>::namespaced(client, &namespace))
}

#[async_trait::async_trait]
impl CredentialStore for KubernetesCredentials {
    async fn read(&self, reference: &str, version: &str) -> anyhow::Result<Vec<u8>> {
        let secrets = self
            .secrets
            .get_or_try_init(|| async { in_cluster_secrets() })
            .await?;
        ensure!(
            reference.starts_with("wamn-credential-")
                && reference
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
            "invalid Secret name"
        );
        let secret = tokio::time::timeout(REQUEST_TIMEOUT, secrets.get_opt(reference))
            .await
            .context("credential Secret request timed out")??
            .context("Secret is absent")?;
        secret
            .data
            .and_then(|mut data| data.remove(version))
            .map(|bytes| bytes.0)
            .context("credential data is absent")
    }
}

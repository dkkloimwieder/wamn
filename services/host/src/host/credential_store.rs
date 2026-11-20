//! Read connection Secrets through the in-cluster credential store.

use tokio::sync::OnceCell;
use wamn_connection::connection_credentials::CredentialStore;
use wamn_kubernetes::{ErrorType, Store as _, kubernetes::KubernetesStore};

#[derive(Default)]
pub(super) struct KubernetesCredentials {
    store: OnceCell<KubernetesStore>,
}

#[async_trait::async_trait]
impl CredentialStore for KubernetesCredentials {
    async fn read(&self, reference: &str, version: &str) -> anyhow::Result<Vec<u8>> {
        let store = self
            .store
            .get_or_try_init(|| async {
                KubernetesStore::in_cluster().map_err(|error| {
                    let local = error.type_ == Some(ErrorType::NoCluster);
                    let error = anyhow::Error::new(error);
                    if local {
                        let reason = "no credential store in local mode";
                        tracing::warn!("{reason}");
                        error.context(reason)
                    } else {
                        error
                    }
                })
            })
            .await?;
        Ok(store.get(reference, version).await?)
    }
}

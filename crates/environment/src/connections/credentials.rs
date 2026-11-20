//! Read a credential for control operations using the in-cluster store.

use wamn_connection::connection_credentials::CredentialStore;
use wamn_kubernetes::{ErrorType, Store as _, kubernetes::KubernetesStore};

#[derive(Debug)]
pub struct KubernetesCredentials;

#[async_trait::async_trait]
impl CredentialStore for KubernetesCredentials {
    async fn read(&self, reference: &str, version: &str) -> anyhow::Result<Vec<u8>> {
        let store = KubernetesStore::in_cluster().map_err(|error| {
            let local = error.type_ == Some(ErrorType::NoCluster);
            let error = anyhow::Error::new(error);
            if local {
                let reason = "no credential store in local mode";
                tracing::warn!("{reason}");
                error.context(reason)
            } else {
                error
            }
        })?;
        Ok(store.get(reference, version).await?)
    }
}

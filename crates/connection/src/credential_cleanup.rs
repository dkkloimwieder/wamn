//! Remove retired values after their replacement generation ages.

use wamn_registry::connections::CredentialReference;

use crate::connection_credentials::WriteCredentials;

/// Collect retired values without changing their immutable credential metadata.
pub async fn collect(store: &dyn WriteCredentials, credentials: Vec<CredentialReference>) {
    for credential in credentials {
        if let Err(error) = store
            .destroy(&credential.reference, &credential.version)
            .await
        {
            tracing::warn!(%error, "retired credential collection failed");
        }
    }
}

//! GCS adapters for the shared pod metadata service, without ADC key files.

use std::sync::Arc;

use object_store::{
    CredentialProvider,
    gcp::{GcpCredential, GcpSigningCredential},
};
use wamn_gcp_metadata::Metadata;

#[derive(Debug)]
pub(crate) struct PodToken(Arc<Metadata>);
#[derive(Debug)]
pub(crate) struct PodSigner(Arc<Metadata>);

pub(crate) fn providers() -> anyhow::Result<(Arc<PodToken>, Arc<PodSigner>)> {
    let metadata = Metadata::shared()?;
    Ok((
        Arc::new(PodToken(Arc::clone(&metadata))),
        Arc::new(PodSigner(metadata)),
    ))
}

fn error(source: anyhow::Error) -> object_store::Error {
    object_store::Error::Generic {
        store: "GCS pod identity",
        source: source.into_boxed_dyn_error(),
    }
}

#[async_trait::async_trait]
impl CredentialProvider for PodToken {
    type Credential = GcpCredential;
    async fn get_credential(&self) -> object_store::Result<Arc<GcpCredential>> {
        Ok(Arc::new(GcpCredential {
            bearer: self.0.token().await.map_err(error)?.to_string(),
        }))
    }
}

#[async_trait::async_trait]
impl CredentialProvider for PodSigner {
    type Credential = GcpSigningCredential;
    async fn get_credential(&self) -> object_store::Result<Arc<GcpSigningCredential>> {
        // A missing private key forces the object_store IAM signBlob path.
        Ok(Arc::new(GcpSigningCredential {
            email: self.0.email().await.map_err(error)?.to_owned(),
            private_key: None,
        }))
    }
}

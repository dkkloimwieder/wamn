//! GCS adapters for the shared pod metadata service, without ADC key files.

use std::sync::Arc;

use object_store::{
    CredentialProvider,
    gcp::{GcpCredential, GcpSigningCredential},
};
use wamn_package_artifact::metadata::Metadata;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pod_signing_credentials_never_contain_a_private_key() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 1024];
            stream.read(&mut request).await.unwrap();
            let email = "pod@example.iam.gserviceaccount.com";
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{email}",
                        email.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let signer = PodSigner(Arc::new(Metadata::with_test_endpoint(endpoint).unwrap()));
        let credential = signer.get_credential().await.unwrap();
        assert!(credential.email.contains('@'));
        assert!(credential.private_key.is_none());
        server.await.unwrap();
    }
}

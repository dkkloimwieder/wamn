//! GCS authentication from the pod metadata identity, without ADC key files.

use std::{
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use object_store::{
    CredentialProvider,
    gcp::{GcpCredential, GcpSigningCredential},
};
use tokio::sync::{Mutex, OnceCell};

const METADATA: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/";

pub(super) struct PodToken {
    client: reqwest::Client,
    cached: Mutex<Option<(Instant, Arc<GcpCredential>)>>,
}
impl fmt::Debug for PodToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PodToken")
    }
}
#[derive(Debug)]
pub(super) struct PodSigner {
    client: reqwest::Client,
    email: OnceCell<String>,
}

pub(super) fn providers() -> Result<(Arc<PodToken>, Arc<PodSigner>), reqwest::Error> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()?;
    Ok((
        Arc::new(PodToken {
            client: client.clone(),
            cached: Mutex::new(None),
        }),
        Arc::new(PodSigner {
            client,
            email: OnceCell::new(),
        }),
    ))
}
fn error(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> object_store::Error {
    object_store::Error::Generic {
        store: "GCS pod identity",
        source: source.into(),
    }
}
async fn metadata(
    client: &reqwest::Client,
    resource: &str,
) -> object_store::Result<reqwest::Response> {
    client
        .get(format!("{METADATA}{resource}"))
        .header("Metadata-Flavor", "Google")
        .send()
        .await
        .map_err(error)?
        .error_for_status()
        .map_err(error)
}
#[derive(serde::Deserialize)]
struct Token {
    access_token: String,
    expires_in: u64,
}
#[async_trait::async_trait]
impl CredentialProvider for PodToken {
    type Credential = GcpCredential;
    async fn get_credential(&self) -> object_store::Result<Arc<GcpCredential>> {
        let mut cached = self.cached.lock().await;
        if let Some((expires, token)) = cached.as_ref()
            && *expires > Instant::now()
        {
            return Ok(Arc::clone(token));
        }
        let response = metadata(&self.client, "token").await?;
        let token: Token =
            serde_json::from_slice(&response.bytes().await.map_err(error)?).map_err(error)?;
        if token.access_token.is_empty() {
            return Err(error("pod metadata returned an empty token"));
        }
        let expires =
            Instant::now() + Duration::from_secs(token.expires_in.saturating_sub(30).min(3600));
        let credential = Arc::new(GcpCredential {
            bearer: token.access_token,
        });
        *cached = Some((expires, Arc::clone(&credential)));
        Ok(credential)
    }
}
#[async_trait::async_trait]
impl CredentialProvider for PodSigner {
    type Credential = GcpSigningCredential;
    async fn get_credential(&self) -> object_store::Result<Arc<GcpSigningCredential>> {
        let email = self
            .email
            .get_or_try_init(|| async {
                let email = metadata(&self.client, "email")
                    .await?
                    .text()
                    .await
                    .map_err(error)?;
                let email = email.trim();
                if email.is_empty() || !email.contains('@') {
                    return Err(error("pod metadata returned no service account email"));
                }
                Ok(email.to_owned())
            })
            .await?;
        // A missing private key forces the object_store IAM signBlob path.
        Ok(Arc::new(GcpSigningCredential {
            email: email.clone(),
            private_key: None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pod_signing_credentials_never_contain_a_private_key() {
        let signer = PodSigner {
            client: reqwest::Client::new(),
            email: OnceCell::from("pod@example.iam.gserviceaccount.com".to_owned()),
        };
        let credential = signer.get_credential().await.unwrap();
        assert_eq!(credential.email, "pod@example.iam.gserviceaccount.com");
        assert!(credential.private_key.is_none());
    }
}

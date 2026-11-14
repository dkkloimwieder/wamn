//! One process service for the pod's metadata access token and account email.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use hyper::Request;
use tokio::sync::{Mutex, OnceCell};

use crate::http_client::HttpClient;
use crate::registry_credentials::METADATA_HOST_ENV;

const TIMEOUT: Duration = Duration::from_secs(5);
const BODY_LIMIT: usize = 65_536;

#[derive(Debug)]
pub(crate) struct InvalidToken {
    pub incomplete: bool,
}
impl std::fmt::Display for InvalidToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid pod metadata access token")
    }
}
impl std::error::Error for InvalidToken {}

#[derive(serde::Deserialize)]
struct Token {
    access_token: String,
    expires_in: u64,
}

/// Token and email reads share a client and serialize refreshes.
pub struct Metadata {
    client: HttpClient,
    endpoint: String,
    token: Mutex<Option<(Instant, Arc<str>)>>,
    email: OnceCell<String>,
}

impl std::fmt::Debug for Metadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Metadata").finish_non_exhaustive()
    }
}

impl Metadata {
    /// The service shared by registry, GCS and KMS in this process.
    pub fn shared() -> anyhow::Result<Arc<Self>> {
        static SERVICE: OnceLock<Arc<Metadata>> = OnceLock::new();
        if let Some(service) = SERVICE.get() {
            return Ok(Arc::clone(service));
        }
        let host = std::env::var(METADATA_HOST_ENV)
            .ok()
            .filter(|host| !host.is_empty())
            .unwrap_or_else(|| "metadata.google.internal".to_owned());
        let service = Arc::new(Self::new(format!(
            "http://{host}/computeMetadata/v1/instance/service-accounts/default/"
        ))?);
        Ok(Arc::clone(SERVICE.get_or_init(|| service)))
    }

    fn new(endpoint: String) -> anyhow::Result<Self> {
        Ok(Self {
            client: HttpClient::shared()?,
            endpoint,
            token: Mutex::new(None),
            email: OnceCell::new(),
        })
    }

    /// Use a private metadata endpoint without changing process environment.
    #[cfg(feature = "test-util")]
    pub fn with_test_endpoint(endpoint: String) -> anyhow::Result<Self> {
        Self::new(endpoint)
    }

    async fn read(&self, resource: &str) -> anyhow::Result<Bytes> {
        let request = Request::get(format!("{}{resource}", self.endpoint))
            .header("Metadata-Flavor", "Google")
            .body(Bytes::new())?;
        let response = self.client.send(request, TIMEOUT, BODY_LIMIT).await?;
        anyhow::ensure!(
            response.status().is_success(),
            "pod metadata request refused"
        );
        Ok(response.into_body())
    }

    /// Get an access token, refreshing thirty seconds before its deadline.
    pub async fn token(&self) -> anyhow::Result<Arc<str>> {
        let mut cached = self.token.lock().await;
        if let Some((deadline, token)) = cached.as_ref()
            && Instant::now() < *deadline
        {
            return Ok(Arc::clone(token));
        }
        let started = Instant::now();
        let token: Token = serde_json::from_slice(&self.read("token").await?)
            .map_err(|_| InvalidToken { incomplete: false })?;
        if token.access_token.is_empty()
            || !token
                .access_token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte))
        {
            return Err(InvalidToken { incomplete: true }.into());
        }
        let token_value: Arc<str> = token.access_token.into();
        let deadline = started + Duration::from_secs(token.expires_in.saturating_sub(30).min(3600));
        *cached = Some((deadline, Arc::clone(&token_value)));
        Ok(token_value)
    }

    /// Get the pod's service account email, without a private signing key.
    pub async fn email(&self) -> anyhow::Result<&str> {
        self.email
            .get_or_try_init(|| async {
                let bytes = self.read("email").await?;
                let email = std::str::from_utf8(&bytes)?.trim();
                anyhow::ensure!(
                    !email.is_empty() && email.contains('@'),
                    "pod metadata returned no service account email"
                );
                Ok(email.to_owned())
            })
            .await
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn metadata_serializes_refreshes_and_does_not_reuse_expiring_credentials() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let received = Arc::clone(&requests);
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let received = Arc::clone(&received);
                tokio::spawn(async move {
                    hyper::server::conn::http1::Builder::new()
                        .serve_connection(
                            hyper_util::rt::TokioIo::new(stream),
                            hyper::service::service_fn(
                                move |request: hyper::Request<hyper::body::Incoming>| {
                                    received.fetch_add(1, Ordering::SeqCst);
                                    let body = match request.uri().path() {
                                        "/token" => {
                                            r#"{"access_token":"pod-token","expires_in":3600}"#
                                        }
                                        "/expiring/token" => {
                                            r#"{"access_token":"pod-token","expires_in":30}"#
                                        }
                                        "/email" => "pod@example.iam.gserviceaccount.com",
                                        _ => panic!("unexpected metadata resource"),
                                    };
                                    std::future::ready(Ok::<_, std::convert::Infallible>(
                                        hyper::Response::new(http_body_util::Full::new(
                                            Bytes::from_static(body.as_bytes()),
                                        )),
                                    ))
                                },
                            ),
                        )
                        .await
                        .unwrap();
                });
            }
        });
        let service = Arc::new(Metadata::new(endpoint.clone()).unwrap());
        let mut callers = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let service = Arc::clone(&service);
            callers.spawn(async move { service.token().await.unwrap() });
        }
        let first = callers.join_next().await.unwrap().unwrap();
        while let Some(token) = callers.join_next().await {
            assert!(Arc::ptr_eq(&first, &token.unwrap()));
        }
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "one token refresh is in flight"
        );
        service.email().await.unwrap();
        service.email().await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 2, "the email is read once");
        let expiring = Metadata::new(format!("{endpoint}expiring/")).unwrap();
        let old = expiring.token().await.unwrap();
        assert!(!Arc::ptr_eq(&old, &expiring.token().await.unwrap()));
        assert_eq!(
            requests.load(Ordering::SeqCst),
            4,
            "an expiring token is never reused"
        );
        server.abort();
    }
}

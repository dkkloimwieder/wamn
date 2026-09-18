//! Resend invitation delivery with bounded requests and redacted credentials.

use std::fmt;
use std::time::Duration;

use serde::Serialize;
use zeroize::Zeroizing;

use crate::IdentityServiceError;

/// Validated sender and API credential for invitation delivery.
#[derive(Clone)]
pub struct ResendConfig {
    key: Zeroizing<String>,
    from: String,
}

impl fmt::Debug for ResendConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResendConfig")
            .field("key", &"[REDACTED]")
            .field("from", &self.from)
            .finish()
    }
}

impl ResendConfig {
    /// Refuse empty credentials and malformed sender configuration before startup.
    pub fn new(key: String, from: String) -> Result<Self, IdentityServiceError> {
        let key = Zeroizing::new(key);
        if key.is_empty()
            || key.len() > 1024
            || key.chars().any(char::is_whitespace)
            || from.is_empty()
            || from.len() > 320
            || !from.contains('@')
            || from.chars().any(char::is_control)
        {
            return Err(IdentityServiceError::new("Resend configuration refused"));
        }
        Ok(Self { key, from })
    }
}

#[derive(Debug)]
pub(super) struct Mailer {
    config: ResendConfig,
    client: reqwest::Client,
    #[cfg(any(test, feature = "test-util"))]
    endpoint: Option<String>,
}

#[derive(Serialize)]
struct Email<'a> {
    from: &'a str,
    to: [&'a str; 1],
    subject: &'static str,
    text: &'a str,
}

impl Mailer {
    pub(super) fn new(config: ResendConfig) -> Result<Self, IdentityServiceError> {
        #[cfg(feature = "test-util")]
        if let Some(endpoint) = std::env::var_os("WAMN_TEST_RESEND_ENDPOINT") {
            let refused = || IdentityServiceError::new("test Resend endpoint refused");
            let endpoint = endpoint.into_string().map_err(|_| refused())?;
            let url = reqwest::Url::parse(&endpoint).map_err(|_| refused())?;
            if url.scheme() != "http"
                || url.host_str() != Some("127.0.0.1")
                || url.path() != "/emails"
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || config.key.as_str() != "unused-development-fixture"
            {
                return Err(refused());
            }
            return Ok(Self::fixture(endpoint));
        }
        let client = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| IdentityServiceError::new("Resend client initialization failed"))?;
        Ok(Self {
            config,
            client,
            #[cfg(any(test, feature = "test-util"))]
            endpoint: None,
        })
    }

    #[cfg(any(test, feature = "test-util"))]
    pub(super) fn fixture(endpoint: String) -> Self {
        Self {
            config: ResendConfig::new(
                "fixture-key".into(),
                "WAMN <fixture@example.invalid>".into(),
            )
            .unwrap(),
            client: reqwest::Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            endpoint: Some(endpoint),
        }
    }

    pub(super) async fn invite(
        &self,
        email: &str,
        principal: &str,
        secret: &str,
    ) -> Result<(), IdentityServiceError> {
        let text = Zeroizing::new(format!(
            "You have been invited to WAMN.\n\nPrincipal: {principal}\nInvitation secret: {secret}\n\nEnter this secret in the terminal enrollment prompt. It expires after 24 hours and can be used once. If you did not expect this invitation, ignore this email."
        ));
        let endpoint = "https://api.resend.com/emails";
        #[cfg(any(test, feature = "test-util"))]
        let endpoint = self.endpoint.as_deref().unwrap_or(endpoint);
        self.send(endpoint, email, &text).await
    }

    async fn send(
        &self,
        endpoint: &str,
        email: &str,
        text: &str,
    ) -> Result<(), IdentityServiceError> {
        // Never retry automatically: a timeout can follow provider acceptance.
        let response = self
            .client
            .post(endpoint)
            .bearer_auth(self.config.key.as_str())
            .json(&Email {
                from: &self.config.from,
                to: [email],
                subject: "Your WAMN invitation",
                text,
            })
            .send()
            .await
            .map_err(|_| IdentityServiceError::new("invitation email request failed"))?;
        if !response.status().is_success() {
            return Err(IdentityServiceError::new("invitation email refused"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[tokio::test]
    async fn resend_payload_is_bounded_and_provider_failure_is_redacted() {
        for status in [200, 403, 429, 500, 302] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/emails", listener.local_addr().unwrap());
            let fixture = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut data = Vec::new();
                loop {
                    let mut chunk = [0; 4096];
                    let n = stream.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    data.extend_from_slice(&chunk[..n]);
                    if let Some(end) = data.windows(4).position(|s| s == b"\r\n\r\n") {
                        let headers = std::str::from_utf8(&data[..end]).unwrap().to_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
                        if data.len() >= end + 4 + length {
                            assert!(headers.contains("authorization: bearer test-secret"));
                            let body: serde_json::Value =
                                serde_json::from_slice(&data[end + 4..]).unwrap();
                            assert_eq!(body["from"], "WAMN <fixture@example.invalid>");
                            assert_eq!(body["to"][0], "person@example.invalid");
                            assert_eq!(body["text"], "fixture invitation");
                            break;
                        }
                    }
                }
                stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: 11\r\nLocation: http://untrusted.invalid/\r\nConnection: close\r\n\r\ntest-secret").as_bytes()).await.unwrap();
            });
            let config = ResendConfig::new(
                "test-secret".into(),
                "WAMN <fixture@example.invalid>".into(),
            )
            .unwrap();
            assert!(!format!("{config:?}").contains("test-secret"));
            let mail = Mailer {
                endpoint: None,
                config,
                client: reqwest::Client::builder()
                    .no_proxy()
                    .retry(reqwest::retry::never())
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(2))
                    .build()
                    .unwrap(),
            };
            let result = mail
                .send(&endpoint, "person@example.invalid", "fixture invitation")
                .await;
            if status == 200 {
                assert!(result.is_ok());
            } else {
                let error = result.unwrap_err();
                assert!(!format!("{error:?} {error}").contains("test-secret"));
            }
            fixture.await.unwrap();
        }
    }
}

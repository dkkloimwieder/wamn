//! Invitation delivery through Resend or a private local mail file.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use zeroize::Zeroizing;

use crate::IdentityServiceError;

/// Validated sender and API credential for invitation delivery.
#[derive(Clone)]
pub struct ResendConfig {
    key: Zeroizing<String>,
    from: String,
    invite_url: Option<String>,
}

impl fmt::Debug for ResendConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResendConfig")
            .field("key", &"[REDACTED]")
            .field("from", &self.from)
            .field("invite_url", &self.invite_url)
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
        Ok(Self {
            key,
            from,
            invite_url: None,
        })
    }

    /// Send the invitation as one link to `<base>/invite#<code>`, and the
    /// reset as one link to `<base>/reset#<secret>`. Without a base, each mail
    /// shows the code to paste into the terminal prompt.
    pub fn with_invite_url(mut self, base: &str) -> Result<Self, IdentityServiceError> {
        if !base.starts_with("https://")
            || base.len() > 2048
            || base
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == '#')
        {
            return Err(IdentityServiceError::new("invitation URL refused"));
        }
        self.invite_url = Some(base.trim_end_matches('/').to_owned());
        Ok(self)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum MailConfig {
    Resend(ResendConfig),
    Local(PathBuf),
}

#[derive(Debug)]
pub(super) enum Mailer {
    Resend(ResendMailer),
    Local(tokio::sync::Mutex<tokio::fs::File>),
}

impl Mailer {
    pub(super) fn new(config: MailConfig) -> Result<Self, IdentityServiceError> {
        match config {
            MailConfig::Resend(config) => Ok(Self::Resend(ResendMailer::new(config)?)),
            MailConfig::Local(path) => {
                use std::os::unix::fs::OpenOptionsExt as _;
                let file = std::fs::OpenOptions::new()
                    .append(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)
                    .map_err(|_| IdentityServiceError::new("create local mail file failed"))?;
                Ok(Self::Local(tokio::sync::Mutex::new(
                    tokio::fs::File::from_std(file),
                )))
            }
        }
    }

    #[cfg(any(test, feature = "test-util"))]
    pub(super) fn fixture(endpoint: String) -> Self {
        Self::Resend(ResendMailer::fixture(endpoint))
    }

    pub(super) async fn invite(
        &self,
        email: &str,
        principal: &str,
        secret: &str,
        expires_at: &str,
    ) -> Result<(), IdentityServiceError> {
        match self {
            Self::Resend(mail) => mail.invite(email, principal, secret, expires_at).await,
            Self::Local(file) => {
                record(
                    file,
                    email,
                    "Your WAMN invitation",
                    &Zeroizing::new(invitation_text(None, principal, secret, expires_at)),
                )
                .await
            }
        }
    }

    pub(super) async fn reset(
        &self,
        email: &str,
        secret: &str,
        expires_at: &str,
    ) -> Result<(), IdentityServiceError> {
        match self {
            Self::Resend(mail) => mail.reset(email, secret, expires_at).await,
            Self::Local(file) => {
                record(
                    file,
                    email,
                    "Reset your WAMN password",
                    &Zeroizing::new(reset_text(None, secret, expires_at)),
                )
                .await
            }
        }
    }

    pub(super) async fn password_changed(&self, email: &str) -> Result<(), IdentityServiceError> {
        match self {
            Self::Resend(mail) => mail.password_changed(email).await,
            Self::Local(file) => {
                record(
                    file,
                    email,
                    "Your WAMN password changed",
                    "Your WAMN password changed.",
                )
                .await
            }
        }
    }
}

async fn record(
    file: &tokio::sync::Mutex<tokio::fs::File>,
    email: &str,
    subject: &str,
    text: &str,
) -> Result<(), IdentityServiceError> {
    use tokio::io::AsyncWriteExt as _;
    let mut bytes = Zeroizing::new(
        serde_json::to_vec(&serde_json::json!({"to": email, "subject": subject, "text": text}))
            .map_err(|_| IdentityServiceError::new("encode local mail failed"))?,
    );
    bytes.push(b'\n');
    let mut file = file.lock().await;
    file.write_all(&bytes)
        .await
        .map_err(|_| IdentityServiceError::new("write local mail failed"))?;
    file.flush()
        .await
        .map_err(|_| IdentityServiceError::new("flush local mail failed"))
}

#[derive(Debug)]
pub(super) struct ResendMailer {
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

impl ResendMailer {
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
        expires_at: &str,
    ) -> Result<(), IdentityServiceError> {
        let text = Zeroizing::new(invitation_text(
            self.config.invite_url.as_deref(),
            principal,
            secret,
            expires_at,
        ));
        let endpoint = "https://api.resend.com/emails";
        #[cfg(any(test, feature = "test-util"))]
        let endpoint = self.endpoint.as_deref().unwrap_or(endpoint);
        self.send(endpoint, email, "Your WAMN invitation", &text)
            .await
    }

    pub(super) async fn reset(
        &self,
        email: &str,
        secret: &str,
        expires_at: &str,
    ) -> Result<(), IdentityServiceError> {
        let text = Zeroizing::new(reset_text(
            self.config.invite_url.as_deref(),
            secret,
            expires_at,
        ));
        self.password_email(email, "Reset your WAMN password", &text)
            .await
    }

    pub(super) async fn password_changed(&self, email: &str) -> Result<(), IdentityServiceError> {
        self.password_email(email, "Your WAMN password changed", "Your WAMN password changed. Sign in with your new password. If you did not make this change, contact your administrator immediately.").await
    }

    async fn password_email(
        &self,
        email: &str,
        subject: &'static str,
        text: &str,
    ) -> Result<(), IdentityServiceError> {
        let endpoint = "https://api.resend.com/emails";
        #[cfg(any(test, feature = "test-util"))]
        let endpoint = self.endpoint.as_deref().unwrap_or(endpoint);
        self.send(endpoint, email, subject, text).await
    }

    async fn send(
        &self,
        endpoint: &str,
        email: &str,
        subject: &'static str,
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
                subject,
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

/// `expires_at` is the token row's expiry in UTC, as `YYYY-MM-DD HH:MM`.
fn invitation_text(
    invite_url: Option<&str>,
    principal: &str,
    secret: &str,
    expires_at: &str,
) -> String {
    match invite_url {
        Some(base) => format!(
            "Open this link to set your WAMN password.\n{base}/invite#{principal}:{secret}\nThe link expires at {expires_at} UTC.\n"
        ),
        None => format!(
            "You have been invited to WAMN.\n\nInvitation code: {principal}:{secret}\n\nPaste this complete code into the invitation prompt. No PAT is needed. The link expires at {expires_at} UTC. It can be used once. If you did not expect this invitation, ignore this email."
        ),
    }
}

fn reset_text(invite_url: Option<&str>, secret: &str, expires_at: &str) -> String {
    match invite_url {
        Some(base) => format!(
            "Open this link to reset your WAMN password.\n{base}/reset#{secret}\nThe link expires at {expires_at} UTC.\n"
        ),
        None => format!(
            "Reset your WAMN password.\n\nReset secret: {secret}\n\nEnter your email and this secret in the password reset prompt. The link expires at {expires_at} UTC. It works once. If you did not request this, ignore this email."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[tokio::test]
    async fn local_sender_records_invitation_code_without_a_network_client() {
        use std::os::unix::fs::PermissionsExt as _;
        let path = std::env::temp_dir().join(format!(
            "wamn-local-mail-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mail = Mailer::new(MailConfig::Local(path.clone())).unwrap();
        assert!(matches!(mail, Mailer::Local(_)));
        mail.invite(
            "person@example.invalid",
            "principal",
            "wamn_inv_secret",
            "2026-10-19 12:00",
        )
        .await
        .unwrap();
        mail.reset(
            "person@example.invalid",
            "wamn_reset_secret",
            "2026-10-19 12:00",
        )
        .await
        .unwrap();
        mail.password_changed("person@example.invalid")
            .await
            .unwrap();
        let recorded = std::fs::read_to_string(&path).unwrap();
        let messages: Vec<serde_json::Value> = recorded
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["to"], "person@example.invalid");
        assert_eq!(messages[0]["subject"], "Your WAMN invitation");
        assert!(
            messages[0]["text"]
                .as_str()
                .unwrap()
                .contains("Invitation code: principal:wamn_inv_secret")
        );
        assert!(
            messages[1]["text"]
                .as_str()
                .unwrap()
                .contains("Reset secret: wamn_reset_secret")
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(mail);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn an_invite_url_makes_each_mail_one_line_and_one_link() {
        let config = ResendConfig::new("key".into(), "WAMN <fixture@example.invalid>".into())
            .unwrap()
            .with_invite_url("https://receiving.example.invalid/")
            .unwrap();
        let expires = "2026-09-30 14:39";
        assert_eq!(
            invitation_text(config.invite_url.as_deref(), "p", "wamn_inv_s", expires),
            "Open this link to set your WAMN password.\nhttps://receiving.example.invalid/invite#p:wamn_inv_s\nThe link expires at 2026-09-30 14:39 UTC.\n"
        );
        let code = invitation_text(None, "p", "wamn_inv_s", expires);
        assert!(code.contains("Invitation code: p:wamn_inv_s"));
        assert!(code.contains("The link expires at 2026-09-30 14:39 UTC."));
        assert_eq!(
            reset_text(config.invite_url.as_deref(), "wamn_reset_s", expires),
            "Open this link to reset your WAMN password.\nhttps://receiving.example.invalid/reset#wamn_reset_s\nThe link expires at 2026-09-30 14:39 UTC.\n"
        );
        let code = reset_text(None, "wamn_reset_s", expires);
        assert!(code.contains("Reset secret: wamn_reset_s"));
        assert!(code.contains("The link expires at 2026-09-30 14:39 UTC."));
    }

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
            let mail = ResendMailer {
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
                .send(
                    &endpoint,
                    "person@example.invalid",
                    "Your WAMN invitation",
                    "fixture invitation",
                )
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

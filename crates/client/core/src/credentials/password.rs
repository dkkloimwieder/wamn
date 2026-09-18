//! Interactive password authentication keeps only the resulting session in memory.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::time::Instant;
use zeroize::Zeroizing;

use super::session::{CachedSession, decode_session};
use super::{CredentialError, CredentialProvider, SessionTarget};
use crate::{HttpRequest, Transport};

/// A bounded transient secret that erases its owned bytes on drop.
pub struct SecretInput(Zeroizing<String>);
impl SecretInput {
    /// Accept nonempty input without trimming or normalization.
    ///
    /// # Errors
    /// Refuses input longer than the server's 1,024-byte password limit.
    pub fn new(value: String) -> Result<Self, CredentialError> {
        let value = Zeroizing::new(value);
        if value.is_empty() || value.len() > 1024 {
            return Err(CredentialError::new(
                "secret input must contain 1 to 1024 bytes",
            ));
        }
        Ok(Self(value))
    }
    /// Read the secret only for its immediate authentication request.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretInput([REDACTED])")
    }
}

/// A password-issued session with no renewal credential or PAT fallback.
pub struct PasswordCredentials {
    target: SessionTarget,
    transport: Arc<dyn Transport>,
    cached: Mutex<Option<CachedSession>>,
}
impl fmt::Debug for PasswordCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PasswordCredentials")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}
impl PasswordCredentials {
    /// Select the exact environment before prompting for a password.
    #[must_use]
    pub fn new(target: SessionTarget, transport: Arc<dyn Transport>) -> Self {
        Self {
            target,
            transport,
            cached: Mutex::new(None),
        }
    }
    /// The selected environment, for the login prompt.
    #[must_use]
    pub fn audience(&self) -> &str {
        &self.target.audience
    }

    /// Establish the first password with a principal-bound invitation.
    ///
    /// # Errors
    /// Refuses invalid, expired, or consumed invitations without retaining input.
    pub async fn enroll(
        &self,
        principal_id: &str,
        invitation: SecretInput,
        password: SecretInput,
    ) -> Result<(), CredentialError> {
        #[derive(Serialize)]
        struct Enrollment<'a> {
            principal_id: &'a str,
            invitation: &'a str,
            password: &'a str,
        }
        let body = serde_json::to_vec(&Enrollment {
            principal_id,
            invitation: invitation.expose(),
            password: password.expose(),
        })
        .map_err(|_| CredentialError::new("enrollment input refused"))?;
        drop(invitation);
        drop(password);
        self.request("enroll", body, 204).await.map(|_| ())
    }

    /// List authorized audiences without issuing or retaining a credential.
    ///
    /// # Errors
    /// Refuses failed authentication and malformed discovery responses.
    pub async fn environments(
        &self,
        email: &str,
        password: &SecretInput,
    ) -> Result<Vec<String>, CredentialError> {
        #[derive(Serialize)]
        struct Discovery<'a> {
            email: &'a str,
            password: &'a str,
        }
        #[derive(Deserialize)]
        struct Environment {
            aud: String,
        }
        #[derive(Deserialize)]
        struct Response {
            environments: Vec<Environment>,
        }
        let body = serde_json::to_vec(&Discovery {
            email,
            password: password.expose(),
        })
        .map_err(|_| CredentialError::new("login input refused"))?;
        let response = self.request("environments", body, 200).await?;
        let response: Response = serde_json::from_str(&response)
            .map_err(|_| CredentialError::new("environment response refused"))?;
        Ok(response
            .environments
            .into_iter()
            .map(|environment| environment.aud)
            .collect())
    }

    /// Authenticate explicitly and replace the local session only on success.
    ///
    /// # Errors
    /// Refuses failed login with fixed diagnostics and no automatic retry.
    pub async fn login(&self, email: &str, password: SecretInput) -> Result<(), CredentialError> {
        #[derive(Serialize)]
        struct Login<'a> {
            email: &'a str,
            password: &'a str,
            aud: &'a str,
        }
        let mut cached = self.cached.lock().await;
        *cached = None;
        let body = serde_json::to_vec(&Login {
            email,
            password: password.expose(),
            aud: &self.target.audience,
        })
        .map_err(|_| CredentialError::new("login input refused"))?;
        drop(password);
        let response = self.request("session", body, 200).await?;
        *cached = Some(decode_session(&response)?);
        Ok(())
    }

    /// Clear the local token. Issued tokens retain their existing server validity.
    pub async fn logout(&self) {
        *self.cached.lock().await = None;
    }

    async fn request(
        &self,
        route: &str,
        body: Vec<u8>,
        status: u16,
    ) -> Result<Zeroizing<String>, CredentialError> {
        let mut endpoint = self.target.endpoint.clone();
        endpoint
            .path_segments_mut()
            .expect("validated hierarchical issuer")
            .pop()
            .push("password")
            .push(route);
        let response = tokio::time::timeout(
            Duration::from_secs(15),
            self.transport.send(HttpRequest {
                url: endpoint.to_string(),
                method: "POST".into(),
                headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
                body,
            }),
        )
        .await
        .map_err(|_| CredentialError::new("authentication timed out; try again explicitly"))?
        .map_err(|_| CredentialError::new("authentication transport failed"))?;
        if response.status != status {
            return Err(CredentialError::new(if response.status == 429 {
                "too many attempts; wait before trying again"
            } else {
                "authentication request refused"
            }));
        }
        Ok(Zeroizing::new(response.body))
    }
}
#[async_trait::async_trait]
impl CredentialProvider for PasswordCredentials {
    async fn bearer(&self) -> Result<String, CredentialError> {
        let mut cached = self.cached.lock().await;
        if let Some(session) = cached.as_ref()
            && Instant::now() < session.deadline
            && SystemTime::now() < session.expires_at
        {
            return Ok(session.token.to_string());
        }
        *cached = None;
        Err(CredentialError::new(
            "log in again and explicitly submit the operation",
        ))
    }
}

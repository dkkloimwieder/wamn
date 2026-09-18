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

/// In-memory password sessions with serialized renewal and no PAT fallback.
pub struct PasswordCredentials {
    target: SessionTarget,
    transport: Arc<dyn Transport>,
    cached: Mutex<Option<PasswordSession>>,
}
struct PasswordSession {
    access: CachedSession,
    renewal: SecretInput,
    absolute: SystemTime,
    absolute_deadline: Instant,
    idle_deadline: Instant,
}
fn decode_password_session(body: &str) -> Result<PasswordSession, CredentialError> {
    #[derive(Deserialize)]
    struct Renewal {
        renewal_token: String,
        login_expires_at: u64,
    }
    let response: Renewal =
        serde_json::from_str(body).map_err(|_| CredentialError::new("renewal response refused"))?;
    let renewal = SecretInput::new(response.renewal_token)?;
    let access = decode_session(body)?;
    let absolute = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(response.login_expires_at))
        .ok_or_else(|| CredentialError::new("login expiry refused"))?;
    let remaining = absolute
        .duration_since(SystemTime::now())
        .map_err(|_| CredentialError::new("login expired"))?;
    if access.expires_at > absolute {
        return Err(CredentialError::new("session exceeds login expiry"));
    }
    let absolute_deadline = Instant::now()
        .checked_add(remaining)
        .ok_or_else(|| CredentialError::new("login expiry refused"))?;
    Ok(PasswordSession {
        access,
        renewal,
        absolute,
        absolute_deadline,
        idle_deadline: Instant::now() + Duration::from_mins(30),
    })
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
        *cached = Some(decode_password_session(&response)?);
        Ok(())
    }

    /// Revoke this login and clear local credentials even if the issuer is unreachable.
    ///
    /// # Errors
    /// Reports unconfirmed server revocation. Issued access tokens keep their expiry.
    pub async fn logout(&self) -> Result<(), CredentialError> {
        let mut cached = self.cached.lock().await;
        let Some(session) = cached.take() else {
            return Err(CredentialError::new(
                "Local credentials cleared. No renewal credential remains to confirm server logout.",
            ));
        };
        let body = self.renewal_body(&session)?;
        self.request("logout", body, 204)
            .await
            .map(|_| ())
            .map_err(|_| {
                CredentialError::new(
                    "Local credentials cleared. Server logout could not be confirmed.",
                )
            })
    }

    /// Request an email without revealing whether the account exists.
    ///
    /// # Errors
    /// Reports throttling or transport failure without retrying.
    pub async fn recover(&self, email: &str) -> Result<(), CredentialError> {
        let body = serde_json::to_vec(&serde_json::json!({"email":email})).expect("string JSON");
        self.request("recover", body, 202).await.map(|_| ())
    }

    /// Reset a password without automatically logging in. Returns notification acceptance.
    ///
    /// # Errors
    /// Refuses invalid secrets and failed requests without retrying.
    pub async fn reset(
        &self,
        email: &str,
        secret: SecretInput,
        password: SecretInput,
    ) -> Result<bool, CredentialError> {
        #[derive(Deserialize)]
        struct Reset {
            status: String,
            notification: String,
        }
        *self.cached.lock().await = None;
        let body = serde_json::to_vec(&serde_json::json!({"email":email,"secret":secret.expose(),"password":password.expose()})).expect("string JSON");
        let response = self.request("reset", body, 200).await?;
        let response: Reset = serde_json::from_str(&response)
            .map_err(|_| CredentialError::new("reset response refused; try normal login"))?;
        if response.status != "password_reset" {
            return Err(CredentialError::new(
                "reset response refused; try normal login",
            ));
        }
        Ok(response.notification == "accepted_for_delivery")
    }

    fn renewal_body(&self, session: &PasswordSession) -> Result<Vec<u8>, CredentialError> {
        serde_json::to_vec(&serde_json::json!({"aud":self.target.audience,"renewal_token":session.renewal.expose()})).map_err(|_| CredentialError::new("renewal input refused"))
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
            && Instant::now() < session.access.deadline
            && SystemTime::now() < session.access.expires_at
            && Instant::now() < session.absolute_deadline
            && SystemTime::now() < session.absolute
        {
            return Ok(session.access.token.to_string());
        }
        // Taking the credential before I/O also clears it if renewal is cancelled.
        let session = cached.take().ok_or_else(|| {
            CredentialError::new("log in again and explicitly submit the operation")
        })?;
        if Instant::now() >= session.absolute_deadline
            || SystemTime::now() >= session.absolute
            || Instant::now() >= session.idle_deadline
        {
            return Err(CredentialError::new(
                "log in again and explicitly submit the operation",
            ));
        }
        let response = self
            .request("renew", self.renewal_body(&session)?, 200)
            .await
            .map_err(|_| {
                CredentialError::new(
                    "renewal failed; log in again and explicitly submit the operation",
                )
            })?;
        let mut renewed = decode_password_session(&response)?;
        if renewed.absolute != session.absolute {
            return Err(CredentialError::new(
                "renewal changed login expiry; log in again",
            ));
        }
        renewed.absolute_deadline = session.absolute_deadline;
        let token = renewed.access.token.to_string();
        *cached = Some(renewed);
        Ok(token)
    }
}

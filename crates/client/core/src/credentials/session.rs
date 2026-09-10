//! Exchange a configured PAT for a session without persistent token storage.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::{CredentialError, CredentialProvider};
use crate::{HttpRequest, Transport};

/// The explicitly configured issuer and project-environment audience.
#[derive(Debug, Clone)]
pub struct SessionTarget {
    endpoint: url::Url,
    audience: String,
}

impl SessionTarget {
    /// Bind exchange to the configured HTTPS issuer, independent of route hosts.
    /// An issuer path prefix is preserved before appending `/session`.
    ///
    /// # Errors
    ///
    /// Refuses an empty audience or an issuer with credentials, a query, or a fragment.
    pub fn new(
        issuer: impl AsRef<str>,
        audience: impl Into<String>,
    ) -> Result<Self, CredentialError> {
        let issuer = issuer.as_ref();
        let mut endpoint = url::Url::parse(issuer)
            .map_err(|_| CredentialError::new("the session issuer must be an HTTPS URL"))?;
        if issuer.trim() != issuer
            || endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(CredentialError::new(
                "the session issuer must be HTTPS without credentials, a query, or a fragment",
            ));
        }
        let audience = audience.into();
        if audience.trim().is_empty() {
            return Err(CredentialError::new("the session audience is empty"));
        }
        endpoint
            .path_segments_mut()
            .map_err(|()| CredentialError::new("the session issuer cannot hold a path"))?
            .pop_if_empty()
            .push("session");
        Ok(Self { endpoint, audience })
    }
}

/// An in-memory session with an explicit PAT path for fresh-only operations.
///
/// The supplied transport must enforce HTTPS and refuse redirects for exchanges.
/// Only the configured issuer receives the PAT. Failed calls are never replayed.
pub struct SessionCredentials {
    target: SessionTarget,
    pat: Arc<dyn CredentialProvider>,
    transport: Arc<dyn Transport>,
    cached: Mutex<Option<CachedSession>>,
}

struct CachedSession {
    token: String,
    expires_at: SystemTime,
    deadline: Instant,
}

impl core::fmt::Debug for SessionCredentials {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Providers and transports can contain credentials or response bodies.
        formatter
            .debug_struct("SessionCredentials")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl SessionCredentials {
    /// Keep the PAT source and cache session credentials only in memory.
    #[must_use]
    pub fn new(
        target: SessionTarget,
        pat: Arc<dyn CredentialProvider>,
        transport: Arc<dyn Transport>,
    ) -> Self {
        Self {
            target,
            pat,
            transport,
            cached: Mutex::new(None),
        }
    }

    /// Complete the first exchange before starting the application.
    ///
    /// # Errors
    ///
    /// Refuses a failed exchange without falling back to PAT mode.
    pub async fn login(&self) -> Result<(), CredentialError> {
        self.bearer().await.map(|_| ())
    }

    async fn exchange(&self) -> Result<CachedSession, CredentialError> {
        let pat = self.fresh_bearer().await?;
        let headers = BTreeMap::from([
            ("authorization".to_owned(), format!("Bearer {pat}")),
            ("content-type".to_owned(), "application/json".to_owned()),
        ]);
        let response = self
            .transport
            .send(HttpRequest {
                url: self.target.endpoint.to_string(),
                method: "POST".to_owned(),
                headers,
                body: wamn_execution_contract::canonical_json_bytes(&serde_json::json!({
                    "aud": self.target.audience,
                })),
            })
            .await
            .map_err(|_| CredentialError::new("the session exchange transport failed"))?;
        if response.status != 200 {
            return Err(CredentialError::new("the session exchange was refused"));
        }
        let response: ExchangeResponse = serde_json::from_str(&response.body)
            .map_err(|_| CredentialError::new("the session exchange response is invalid"))?;
        if response.token_type != "Bearer"
            || response.access_token.is_empty()
            || !response
                .access_token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte))
        {
            return Err(CredentialError::new(
                "the session exchange response is invalid",
            ));
        }
        let expires_at = u64::try_from(response.expires_at)
            .ok()
            .and_then(|seconds| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
            .ok_or_else(|| CredentialError::new("the session expiry is invalid"))?;
        let remaining = expires_at
            .duration_since(SystemTime::now())
            .ok()
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| CredentialError::new("the session is already expired"))?;
        let deadline = Instant::now()
            .checked_add(remaining)
            .ok_or_else(|| CredentialError::new("the session expiry is invalid"))?;
        Ok(CachedSession {
            token: response.access_token,
            expires_at,
            deadline,
        })
    }
}

#[async_trait::async_trait]
impl CredentialProvider for SessionCredentials {
    async fn bearer(&self) -> Result<String, CredentialError> {
        // Hold the lock through exchange so concurrent callers share one result.
        let mut cached = self.cached.lock().await;
        if let Some(session) = cached.as_ref()
            && Instant::now() < session.deadline
            && SystemTime::now() < session.expires_at
        {
            return Ok(session.token.clone());
        }
        // Do not keep an expired session after a failed or cancelled renewal.
        *cached = None;
        let session = self.exchange().await?;
        let token = session.token.clone();
        *cached = Some(session);
        Ok(token)
    }

    async fn fresh_bearer(&self) -> Result<String, CredentialError> {
        self.pat
            .fresh_bearer()
            .await
            .map_err(|_| CredentialError::new("a fresh PAT is not available"))
    }
}

#[derive(Deserialize)]
struct ExchangeResponse {
    access_token: String,
    token_type: String,
    expires_at: i64,
}

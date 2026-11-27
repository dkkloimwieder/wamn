//! Shared OAuth token refresh with per-connection leases.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::connection_credentials::ConnectionCredentials;
use wamn_envelope::encryption::Envelope;

pub type CacheKey = (uuid::Uuid, i64);

#[derive(Default)]
pub struct Cache {
    entries: Mutex<Entries>,
}

#[derive(Default)]
struct Entries {
    tokens: HashMap<CacheKey, (String, Instant)>,
}

impl Cache {
    pub fn get(&self, key: &CacheKey) -> Option<String> {
        self.entries
            .lock()
            .expect("OAuth token cache lock")
            .tokens
            .get(key)
            .filter(|(_, deadline)| Instant::now() < *deadline)
            .map(|(token, _)| token.clone())
    }

    pub fn remove(&self, key: &CacheKey) {
        self.entries
            .lock()
            .expect("OAuth token cache lock")
            .tokens
            .remove(key);
    }

    pub fn retain_generation(&self, id: uuid::Uuid, generation: Option<i64>) {
        let mut entries = self.entries.lock().expect("OAuth token cache lock");
        entries
            .tokens
            .retain(|(connection, active), _| *connection != id || generation == Some(*active));
    }
}

/// JSON encrypted in oauth_token.ciphertext, also used by authorization callbacks.
#[derive(Serialize, Deserialize)]
pub struct Token {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(..)")
    }
}

pub struct Fetched {
    pub token: Token,
    pub expires_in: Option<u64>,
}

pub struct Available {
    pub token: String,
}

/// A shared refresh failure. Debug output omits provider and token material.
pub struct Error {
    pub unauthorized: bool,
    connection: uuid::Uuid,
    generation: i64,
    source: anyhow::Error,
}
impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthTokenError")
            .field("connection", &self.connection)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "OAuth token unavailable for connection {} generation {}",
            self.connection, self.generation
        )
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}
pub fn is_oauth(auth_type: &str) -> bool {
    matches!(
        auth_type,
        "oauth2_client_credentials" | "oauth2_client_credentials_jwt" | "oauth2_authorization_code"
    )
}

pub struct Request<'a> {
    pub reader: &'a ConnectionCredentials,
    /// The host-attested executing principal of this call.
    pub principal: wamn_identity_protocol::PrincipalId,
    pub project: &'a wamn_registry::ProjectId,
    pub env: &'a wamn_registry::EnvName,
    pub connection: uuid::Uuid,
    pub generation: i64,
    pub authorization_code: bool,
    /// The caller's deadline for waiting and provider work.
    pub deadline: Instant,
    /// A 401 refreshes only the access token that the provider refused.
    pub rejected: Option<&'a str>,
}

/// Arguments for a refresh of the active generation.
pub struct Refresh<'a> {
    pub principal: wamn_identity_protocol::PrincipalId,
    pub connection: uuid::Uuid,
    pub generation: i64,
    pub authorization_code: bool,
    /// The caller's deadline for waiting and provider work.
    pub deadline: Instant,
    pub rejected: Option<&'a str>,
}

impl std::fmt::Debug for Refresh<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Refresh")
            .field("connection", &self.connection)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// The provider call runs after the lease commits and the connection is returned.
pub type Fetch<'a> = Box<
    dyn FnOnce(
            Option<Token>,
        )
            -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<Fetched>> + Send + 'a>>
        + Send
        + 'a,
>;

impl Error {
    /// No authorization-code token was stored for this connection.
    pub fn authorization_required(request: &Refresh<'_>) -> Self {
        Self {
            unauthorized: true,
            connection: request.connection,
            generation: request.generation,
            source: anyhow::anyhow!("OAuth authorization required"),
        }
    }
}

/// Reuse a stored token or acquire the connection's refresh lease.
pub async fn get<'a, F, Fut>(
    request: Request<'a>,
    envelope: &'a Envelope,
    fetch: F,
) -> Result<Available, Error>
where
    F: FnOnce(Option<Token>) -> Fut + Send + 'a,
    Fut: Future<Output = anyhow::Result<Fetched>> + Send + 'a,
{
    let result = async {
        let database = request
            .reader
            .project_connections(request.project, request.env)?;
        crate::project_connections::oauth_token(
            &database,
            request.project,
            Refresh {
                principal: request.principal,
                connection: request.connection,
                generation: request.generation,
                authorization_code: request.authorization_code,
                deadline: request.deadline,
                rejected: request.rejected,
            },
            envelope,
            Box::new(|old| Box::pin(fetch(old))),
        )
        .await
    }
    .await;
    result.map_err(|source| Error {
        unauthorized: false,
        connection: request.connection,
        generation: request.generation,
        source,
    })?
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Fetched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fetched").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Available {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Available").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Request<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request").finish_non_exhaustive()
    }
}

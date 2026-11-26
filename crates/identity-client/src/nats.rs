//! NATS account and user requests to identity.

use std::path::Path;

use hyper::StatusCode;
use serde::{Serialize, de::DeserializeOwned};
use url::Url;
use wamn_identity_protocol::nats::{
    Account, BrokerIdentity, ENSURE_ACCOUNT_PATH, EnsureAccount, SIGN_HOST_USER_PATH,
    SIGN_USER_PATH, SignHostUser, SignUser, SignedUser,
};

use super::{IdentityHttp, TOKEN_PATH, service_endpoint};

const MAX_RESPONSE_BYTES: usize = 8192;

/// The org worker's client of identity's NATS signing authority.
pub struct NatsClient {
    http: IdentityHttp,
    accounts: Url,
    users: Url,
}

impl std::fmt::Debug for NatsClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("NatsClient").finish_non_exhaustive()
    }
}

impl NatsClient {
    /// Use the projected worker service account token and the identity server CA.
    pub fn new(endpoint: &str, ca: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            http: IdentityHttp::new(ca, Path::new(TOKEN_PATH))?,
            accounts: service_endpoint(endpoint, ENSURE_ACCOUNT_PATH.trim_start_matches('/'))?,
            users: service_endpoint(endpoint, SIGN_USER_PATH.trim_start_matches('/'))?,
        })
    }

    /// Sign and push the environment account's current limits.
    pub async fn ensure_account(&self, request: &EnsureAccount) -> anyhow::Result<Account> {
        post(&self.http, &self.accounts, request).await
    }

    /// Sign the caller's public key for the requested user purpose and scope.
    pub async fn sign_user(&self, request: &SignUser) -> anyhow::Result<SignedUser> {
        post(&self.http, &self.users, request).await
    }
}

/// Sign a public key using the host's projected service account token.
pub async fn sign_host_user(
    endpoint: &str,
    ca: &Path,
    request: &SignHostUser,
) -> anyhow::Result<SignedUser> {
    let http = IdentityHttp::new(ca, Path::new(TOKEN_PATH))?;
    let url = service_endpoint(endpoint, SIGN_HOST_USER_PATH.trim_start_matches('/'))?;
    post(&http, &url, request).await
}

async fn post<T: DeserializeOwned>(
    http: &IdentityHttp,
    endpoint: &Url,
    request: &impl Serialize,
) -> anyhow::Result<T> {
    let mut response = http.post(endpoint, serde_json::to_vec(request)?).await?;
    anyhow::ensure!(
        response.status() != StatusCode::CONFLICT,
        "NATS environment limits conflict with usage or store capacity"
    );
    anyhow::ensure!(
        response.status() == StatusCode::OK,
        "identity NATS request failed: {}",
        response.status()
    );
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            chunk.len() <= MAX_RESPONSE_BYTES - body.len(),
            "identity NATS response exceeds the size limit"
        );
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| anyhow::anyhow!("invalid identity NATS response"))
}

#[async_trait::async_trait]
impl BrokerIdentity for NatsClient {
    async fn ensure_account(&self, request: &EnsureAccount) -> anyhow::Result<Account> {
        self.ensure_account(request).await
    }

    async fn sign_user(&self, request: &SignUser) -> anyhow::Result<SignedUser> {
        self.sign_user(request).await
    }
}

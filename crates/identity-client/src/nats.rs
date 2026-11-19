//! Control's NATS account and user requests to identity.

use std::path::Path;

use hyper::StatusCode;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::Url;
use wamn_registry::{EnvName, OrgId, ProjectId};

use super::{IdentityHttp, TOKEN_PATH, service_endpoint};

const MAX_RESPONSE_BYTES: usize = 8192;

/// Account provisioning and resolver publication route.
pub const ENSURE_ACCOUNT_PATH: &str = "/nats/accounts";
/// User JWT signing route.
pub const SIGN_USER_PATH: &str = "/nats/users";

/// Ensure one environment account with a disk reservation for its project count.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnsureAccount {
    pub org: OrgId,
    pub env: EnvName,
    pub project_count: u32,
}

/// The environment account that the NATS resolver accepted.
#[derive(Debug, Serialize, Deserialize)]
pub struct Account {
    pub public_key: String,
}

/// Select the permissions identity grants to the user's public key.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum UserKind {
    Workflow,
    Ingress,
    WorkflowNative,
    Cdc { project: ProjectId },
    Recovery { project: ProjectId },
    StreamAdmin,
    HostScheduler,
    HostData,
}

/// Sign a NATS user for the authenticated org. The caller retains the seed.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignUser {
    pub org: OrgId,
    pub env: EnvName,
    pub kind: UserKind,
    pub public_key: String,
}

/// The signed user JWT and the inbox prefix the client must use.
#[derive(Serialize, Deserialize)]
pub struct SignedUser {
    pub jwt: String,
    pub inbox_prefix: String,
}

impl std::fmt::Debug for SignedUser {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("SignedUser").finish_non_exhaustive()
    }
}

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
        self.post(&self.accounts, request).await
    }

    /// Sign the caller's public key for the requested user kind and scope.
    pub async fn sign_user(&self, request: &SignUser) -> anyhow::Result<SignedUser> {
        self.post(&self.users, request).await
    }

    async fn post<T: DeserializeOwned>(
        &self,
        endpoint: &Url,
        request: &impl Serialize,
    ) -> anyhow::Result<T> {
        let mut response = self
            .http
            .post(endpoint, serde_json::to_vec(request)?)
            .await?;
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
}

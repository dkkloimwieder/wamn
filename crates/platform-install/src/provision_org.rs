//! Provision an org's system database logins, registry row, and local OAuth key.

use ring::rand::{SecureRandom as _, SystemRandom};
use std::fmt;
use wamn_postgres::database::ConnectionConfig;
use wamn_registry::OrgId;
use wamn_role::LoginName;

pub const CONTROL_SECRET: &str = "wamn-control-db";
pub const APPLY_SECRET: &str = "wamn-apply-db";
const OAUTH_SECRET: &str = "wamn-oauth";
pub const ORG_INPUTS_DIR: &str = "/etc/wamn/org-inputs";
pub const OAUTH_ENCRYPTION_INPUT: &str = "oauthEncryption";
pub const SQL_ACCOUNT: &str = "wamn-org-sql";
pub const INVITATION_ACCOUNT: &str = "wamn-org-invitation";
pub const CONTROL_AUDIENCE: &str = "wamn-control";
pub const CONTROL_TOKEN_PATH: &str = "/var/run/secrets/wamn-control/token";
pub const FIRST_OWNER_PATH: &str = "/internal/first-owner";

pub fn control_login(org: &OrgId, database: &str) -> LoginName {
    wamn_registry::name::credential_login(
        wamn_role::CONTROL_ROLE,
        &wamn_registry::control_identity_hash(org, database),
    )
}

/// The system database login used for this org's operations.
pub fn apply_login(org: &OrgId, database: &str) -> LoginName {
    wamn_registry::name::credential_login(
        wamn_role::APPLY_ROLE,
        &wamn_registry::apply_identity_hash(org, database),
    )
}

/// Acquire a dedicated session before changing the org logins or their Secrets.
pub trait OrgDatabase {
    type Locked<'a>: LockedOrg + Send
    where
        Self: 'a;

    fn lock_org<'a>(
        &'a self,
        database: &'a ConnectionConfig,
        org: &'a OrgId,
    ) -> impl Future<Output = anyhow::Result<Self::Locked<'a>>> + Send;
}

/// The lock remains held until this session is dropped.
pub trait LockedOrg {
    fn rotate_logins(&mut self) -> impl Future<Output = anyhow::Result<Vec<OrgLogin>>> + Send;
}

/// Read and fill the Secrets created by the org chart.
pub trait OrgSecrets {
    fn contains_key(
        &self,
        org: &OrgId,
        secret: &str,
        key: &str,
    ) -> impl Future<Output = anyhow::Result<bool>> + Send;

    fn write_key(
        &self,
        org: &OrgId,
        secret: &str,
        key: &str,
        value: &[u8],
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// Initialize the local OAuth key if needed, record the org, and rotate both logins.
pub async fn provision_org(
    database: &ConnectionConfig,
    org: &OrgId,
    oauth_encryption: &serde_json::Value,
    store: &impl OrgDatabase,
    secrets: &impl OrgSecrets,
) -> anyhow::Result<()> {
    let mut locked = store.lock_org(database, org).await?;
    if oauth_encryption["provider"] == "local"
        && !secrets.contains_key(org, OAUTH_SECRET, "key").await?
    {
        // The host's envelope reader requires 32 raw bytes.
        let mut key = [0_u8; 32];
        SystemRandom::new()
            .fill(&mut key)
            .map_err(|_| anyhow::anyhow!("generate the OAuth encryption key for org {org}"))?;
        secrets.write_key(org, OAUTH_SECRET, "key", &key).await?;
    }

    for login in locked.rotate_logins().await? {
        secrets
            .write_key(org, login.secret, "password", login.password.as_bytes())
            .await?;
    }
    drop(locked);
    Ok(())
}

/// One org login and the password that provisioning gave it.
pub struct OrgLogin {
    pub login: LoginName,
    /// The org Secret that carries the password.
    pub secret: &'static str,
    pub password: String,
}

/// Names the login only: the password must not reach a log.
impl fmt::Debug for OrgLogin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OrgLogin")
            .field("login", &self.login)
            .field("secret", &self.secret)
            .finish_non_exhaustive()
    }
}

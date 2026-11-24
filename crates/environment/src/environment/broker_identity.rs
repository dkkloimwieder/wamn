//! Account administration and user signing for the deployment worker.

use wamn_identity_protocol::nats::{Account, EnsureAccount, SignUser, SignedUser};

#[async_trait::async_trait]
pub trait BrokerIdentity: Send + Sync {
    async fn ensure_account(&self, request: &EnsureAccount) -> anyhow::Result<Account>;
    async fn sign_user(&self, request: &SignUser) -> anyhow::Result<SignedUser>;
}

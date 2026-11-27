//! Event stream and recovery operations used by deployment.

use std::fmt::Debug;

use chrono::{DateTime, Utc};
use wamn_registry::Triple;

pub const CA_FILE: &str = "/etc/wamn/nats-ca/ca.crt";
pub const CREDENTIALS_KEY: &str = "user.creds";

/// Stream placement and duplicate detection policy.
#[derive(Debug, Clone)]
pub struct EventBroker {
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
}

#[async_trait::async_trait]
pub trait DeploymentBroker: Debug + Send + Sync {
    fn credentials_inbox(&self, credentials: &str) -> anyhow::Result<String>;

    async fn provision(
        &self,
        url: &str,
        credentials: &str,
        triple: &Triple,
        policy: &EventBroker,
    ) -> anyhow::Result<()>;

    async fn remove(&self, url: &str, credentials: &str, stream: &str) -> anyhow::Result<bool>;

    async fn last_capture_event(
        &self,
        url: &str,
        credentials: &str,
        stream: &str,
        triple: &Triple,
    ) -> anyhow::Result<Option<(u64, DateTime<Utc>)>>;
}

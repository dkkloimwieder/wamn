//! NATS connections, event streams, and shared memory buckets.

pub mod advisories;
pub mod auth_changes;
pub mod authorization;
pub mod connection_changes;
pub mod events;
pub mod kv_memory;
pub mod nats;
pub mod streams;

/// A committed change set names its bucket and the keys to remove.
pub trait Changes {
    fn into_deletions(self) -> (&'static str, Vec<kv_memory::Deletion>);
}

/// Publish committed changes on one shared broker connection.
#[derive(Debug, Clone, Default)]
pub struct Publisher(Option<async_nats::Client>);

impl Publisher {
    pub fn new(client: async_nats::Client) -> Self {
        Self(Some(client))
    }
    pub async fn connect(connection: &nats::PlatformNats) -> anyhow::Result<Self> {
        Ok(Self::new(connection.connect().await?))
    }
    pub fn client(&self) -> Option<&async_nats::Client> {
        self.0.as_ref()
    }
    pub async fn publish(&self, changes: impl Changes) {
        if let Some(client) = &self.0 {
            let (bucket, deletions) = changes.into_deletions();
            kv_memory::delete(client, bucket, deletions).await;
        }
    }
}

/// The event broker inputs of CDC (`enable-cdc-project-env`).
#[derive(Debug, Clone)]
pub struct EventBroker {
    /// The NATS connection with the client certificate (docs/plan/alignment.md R4).
    pub nats: nats::PlatformNats,
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
}

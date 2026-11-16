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
pub trait Changes {}

/// Publish committed changes on one shared broker connection.
#[derive(Debug, Clone, Default)]
pub struct Publisher {}

impl Publisher {
    pub fn new(_client: async_nats::Client) -> Self {
        Self::default()
    }
    pub async fn connect(_connection: &nats::PlatformNats) -> anyhow::Result<Self> {
        Ok(Self::default())
    }
    pub async fn publish(&self, _changes: impl Changes) {}
}

/// The event broker inputs of CDC (`enable-cdc-project-env`).
#[derive(Debug, Clone)]
pub struct EventBroker {
    /// The NATS connection with the client certificate (docs/plan/alignment.md R4).
    pub nats: nats::PlatformNats,
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
}

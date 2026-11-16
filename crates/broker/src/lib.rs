//! NATS connections, event streams, and shared memory buckets.

pub mod authorization;
pub mod events;
pub mod kv_memory;
pub mod nats;
pub mod streams;

/// The event broker inputs of CDC (`enable-cdc-project-env`).
#[derive(Debug, Clone)]
pub struct EventBroker {
    /// The NATS connection with the client certificate (docs/plan/alignment.md R4).
    pub nats: nats::PlatformNats,
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
}

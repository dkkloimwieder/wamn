//! NATS connections, event streams, and shared memory buckets.

pub mod events;
pub mod kv_memory;
pub mod nats;
pub mod publisher;
pub mod streams;

/// The event broker inputs of CDC (`enable-cdc-project-env`).
#[derive(Debug, Clone)]
pub struct EventBroker {
    pub stream_replicas: usize,
    pub dup_window_secs: u64,
}

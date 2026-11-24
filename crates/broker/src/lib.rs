//! NATS connections, event streams, and shared memory buckets.

pub mod events;
pub mod kv_memory;
pub mod nats;
pub mod publisher;
pub mod streams;

pub mod deployment;
pub use wamn_event::deploy_broker::EventBroker;

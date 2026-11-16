//! The NATS KV buckets that back the host memories
//! (docs/architecture/execution.md#auth-memory and #connection-memory).
//!
//! Postgres owns every fact a memory keeps. A writer deletes the keys of the
//! facts its commit changed, and the TTL bounds a lost delete.

use std::time::Duration;

/// Retention for connection facts. Auth revocation bounds belong to project configuration.
pub const CONNECTION_MEMORY_TTL: Duration = Duration::from_secs(60);

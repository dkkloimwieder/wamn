//! The NATS KV buckets that back the host memories
//! (docs/architecture/execution.md#auth-memory and #connection-memory).
//!
//! Postgres owns every fact a memory keeps. A writer deletes the keys of the
//! facts its commit changed, and the TTL bounds a lost delete.

use std::time::Duration;

/// Retention for connection facts. Auth revocation bounds belong to project configuration.
pub const CONNECTION_MEMORY_TTL: Duration = Duration::from_secs(60);

// Auth facts have different revocation bounds. Bound storage by bytes instead of a shared age.
const AUTH_MEMORY_BYTES: i64 = 1 << 20;

/// Open `bucket`. Only identity creates a bucket ([`create_buckets`]), so a
/// bucket is absent until identity runs and after a NATS restart.
pub async fn open_bucket(
    client: &async_nats::Client,
    bucket: &str,
) -> anyhow::Result<async_nats::jetstream::kv::Store> {
    use anyhow::Context as _;
    async_nats::jetstream::new(client.clone())
        .get_key_value(bucket)
        .await
        .with_context(|| format!("open the NATS KV bucket {bucket}"))
}

/// Make each memory bucket, or bring its configuration up to date
/// (docs/plan/alignment.md R7).
pub async fn create_buckets(client: &async_nats::Client) -> anyhow::Result<()> {
    for bucket in [
        crate::auth_changes::BUCKET,
        crate::connection_changes::BUCKET,
    ] {
        create_bucket(client, bucket).await?;
    }
    Ok(())
}

async fn create_bucket(
    client: &async_nats::Client,
    bucket: &str,
) -> anyhow::Result<async_nats::jetstream::kv::Store> {
    use anyhow::Context as _;
    async_nats::jetstream::new(client.clone())
        .create_or_update_key_value(async_nats::jetstream::kv::Config {
            bucket: bucket.to_owned(),
            history: 1,
            max_age: if bucket == crate::connection_changes::BUCKET {
                CONNECTION_MEMORY_TTL
            } else {
                Duration::ZERO
            },
            max_bytes: if bucket == crate::auth_changes::BUCKET {
                AUTH_MEMORY_BYTES
            } else {
                0
            },
            storage: async_nats::jetstream::stream::StorageType::Memory,
            ..Default::default()
        })
        .await
        .with_context(|| format!("make the NATS KV bucket {bucket}"))
}

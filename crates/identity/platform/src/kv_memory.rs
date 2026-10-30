//! The NATS KV buckets that back the host memories
//! (docs/architecture/execution.md#auth-memory and #connection-memory).
//!
//! Postgres owns every fact a memory keeps. A writer deletes the keys of the
//! facts its commit changed, and the TTL bounds a lost delete.

use std::time::Duration;

/// How long a bucket keeps a value, and a host its copy: the backstop for a
/// lost delete.
pub const MEMORY_TTL: Duration = Duration::from_secs(60);

/// The keys one change deletes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deletion {
    /// One key.
    Key(String),
    /// Every key whose first token is this one, such as every key of an
    /// environment.
    Under(String),
}

/// Open `bucket`, and make it when it is absent: the scheduler NATS keeps
/// JetStream in memory of its pod, so a restart loses the bucket.
pub async fn open_bucket(
    client: &async_nats::Client,
    bucket: &str,
) -> anyhow::Result<async_nats::jetstream::kv::Store> {
    use anyhow::Context as _;
    async_nats::jetstream::new(client.clone())
        .create_or_update_key_value(async_nats::jetstream::kv::Config {
            bucket: bucket.to_owned(),
            history: 1,
            max_age: MEMORY_TTL,
            storage: async_nats::jetstream::stream::StorageType::Memory,
            ..Default::default()
        })
        .await
        .with_context(|| format!("open the NATS KV bucket {bucket}"))
}

/// Delete `deletions` from `bucket` after the commit that made them. A failed
/// delete is logged, because the commit stands and the TTL bounds it. Each
/// call opens the bucket, so a writer outlives a restart of the scheduler
/// NATS.
pub async fn delete(client: &async_nats::Client, bucket: &str, deletions: Vec<Deletion>) {
    if deletions.is_empty() {
        return;
    }
    let store = match open_bucket(client, bucket).await {
        Ok(store) => store,
        Err(error) => {
            tracing::warn!(
                error = format!("{error:#}"),
                bucket,
                "the memory changes were not deleted; the TTL bounds them"
            );
            return;
        }
    };
    for deletion in deletions {
        if let Err(error) = delete_one(&store, &deletion).await {
            tracing::warn!(
                error = format!("{error:#}"),
                bucket,
                ?deletion,
                "the memory change was not deleted; the TTL bounds it"
            );
        }
    }
}

async fn delete_one(
    store: &async_nats::jetstream::kv::Store,
    deletion: &Deletion,
) -> anyhow::Result<()> {
    use futures_util::TryStreamExt as _;
    let keys = match deletion {
        Deletion::Key(key) => vec![key.clone()],
        Deletion::Under(token) => {
            let mut subjects = store
                .stream
                .info_with_subjects(format!("{}{token}.>", store.prefix))
                .await?;
            let mut keys = Vec::new();
            while let Some((subject, _)) = subjects.try_next().await? {
                if let Some(key) = subject.strip_prefix(&store.prefix) {
                    keys.push(key.to_owned());
                }
            }
            keys
        }
    };
    for key in keys {
        store.delete(&key).await?;
    }
    Ok(())
}

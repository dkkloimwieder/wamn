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

/// How often identity makes sure that the buckets exist. The NATS keeps the
/// buckets in memory, so a restart loses them.
pub const BUCKET_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Make each memory bucket, or bring its configuration up to date
/// (docs/plan/alignment.md R7). Identity calls this on start and every
/// [`BUCKET_POLL_INTERVAL`].
pub async fn create_buckets(client: &async_nats::Client) -> anyhow::Result<()> {
    for bucket in [
        crate::auth_changes::BUCKET,
        crate::connection_changes::BUCKET,
    ] {
        create_bucket(client, bucket).await?;
    }
    Ok(())
}

/// Make each memory bucket on start, and again every [`BUCKET_POLL_INTERVAL`], for as
/// long as the task runs.
pub fn keep_buckets(client: async_nats::Client) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut poll = tokio::time::interval(BUCKET_POLL_INTERVAL);
        loop {
            poll.tick().await;
            if let Err(error) = create_buckets(&client).await {
                tracing::warn!(
                    error = format!("{error:#}"),
                    "the memory buckets were not made; hosts cache nothing until they are"
                );
            }
        }
    })
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
            max_age: MEMORY_TTL,
            storage: async_nats::jetstream::stream::StorageType::Memory,
            ..Default::default()
        })
        .await
        .with_context(|| format!("make the NATS KV bucket {bucket}"))
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

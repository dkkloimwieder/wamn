//! The local replica of one NATS KV bucket of facts that Postgres owns
//! (docs/architecture/execution.md#auth-memory and #connection-memory).
//!
//! A host that reads a fact from Postgres puts it with the revision of the
//! key from before the read, so a delete in between refuses the put. The host
//! watches the keys it serves and keeps each value it sees. The bucket keeps
//! a value for at most the TTL, and so does the replica. A host keeps facts
//! only while it watches the bucket, because only the watch brings the
//! deletes. A host without the bucket, or with a broken watch, keeps no fact.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, Instant, SystemTime};

use async_nats::jetstream::kv::{Entry as KvEntry, Operation, Store};
use futures_util::StreamExt as _;

/// A map that grows past this many entries drops its expired ones.
pub(crate) const SWEEP_AT: usize = 10_000;

/// How long a put to the bucket may take before the host answers without
/// keeping the fact.
const PUT_TIMEOUT: Duration = Duration::from_secs(1);

/// The revision of a key that a host saw before it read the fact of the key.
/// A put with it fails when a writer changed the key during the read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket(pub(crate) u64);

struct Entry<A> {
    revision: u64,
    seen: Instant,
    /// `None` after a delete.
    answer: Option<A>,
}

/// The replica of one bucket, with answers of type `A`.
pub(crate) struct KvMemory<A> {
    name: &'static str,
    ttl: Duration,
    entries: Mutex<HashMap<String, Entry<A>>>,
    /// The bucket, while the host watches it. Without it, the host keeps no
    /// fact.
    bucket: RwLock<Option<Store>>,
}

impl<A> KvMemory<A>
where
    A: Clone + serde::Serialize + serde::de::DeserializeOwned,
{
    /// A replica of the bucket `name` whose facts live for `ttl`.
    pub(crate) fn new(name: &'static str, ttl: Duration) -> Self {
        Self {
            name,
            ttl,
            entries: Mutex::new(HashMap::new()),
            bucket: RwLock::new(None),
        }
    }

    pub(crate) fn ttl(&self) -> Duration {
        self.ttl
    }

    fn entries(&self) -> MutexGuard<'_, HashMap<String, Entry<A>>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn bucket(&self) -> Option<Store> {
        self.bucket
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn set_bucket(&self, bucket: Option<Store>) {
        *self.bucket.write().unwrap_or_else(PoisonError::into_inner) = bucket;
    }

    /// Drop every fact.
    pub(crate) fn clear(&self) {
        self.entries().clear();
    }

    #[cfg(test)]
    pub(crate) fn seed(&self, key: String, answer: A) {
        self.entries().insert(
            key,
            Entry {
                revision: 1,
                seen: Instant::now(),
                answer: Some(answer),
            },
        );
    }

    /// The live answer of `key`, or the ticket of a read of it.
    /// Replica expiry uses the original publication time.
    pub(crate) fn answer(&self, key: &str) -> Result<A, Ticket> {
        match self.entries().get(key) {
            Some(entry) if entry.seen.elapsed() < self.ttl => {
                entry.answer.clone().ok_or(Ticket(entry.revision))
            }
            Some(entry) => Err(Ticket(entry.revision)),
            None => Err(Ticket(0)),
        }
    }

    /// The ticket of a read of `key`.
    pub(crate) fn ticket(&self, key: &str) -> Ticket {
        self.entries()
            .get(key)
            .map_or(Ticket(0), |entry| Ticket(entry.revision))
    }

    fn keep_entry(&self, key: String, revision: u64, answer: Option<A>, age: Duration) {
        // The caller holds the entries during the test, so a fact kept here
        // falls to the clear that follows a broken watch.
        let mut entries = self.entries();
        if self.bucket().is_none() {
            return;
        }
        if entries.len() >= SWEEP_AT {
            let ttl = self.ttl;
            entries.retain(|_, entry| entry.seen.elapsed() < ttl);
        }
        let entry = entries.entry(key).or_insert(Entry {
            revision: 0,
            seen: Instant::now(),
            answer: None,
        });
        // The watch also delivers the revision this host put.
        if revision >= entry.revision {
            entry.revision = revision;
            entry.seen = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
            entry.answer = answer.filter(|_| age < self.ttl);
        }
    }

    /// Put `answer` at `key` if the key still has the revision of `ticket`,
    /// and keep it. Without the bucket, keep nothing.
    pub(crate) async fn put(&self, key: String, ticket: Ticket, answer: A) {
        if self.ttl.is_zero() {
            return;
        }
        let Some(bucket) = self.bucket() else {
            return;
        };
        let bytes = match serde_json::to_vec(&answer) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(%error, bucket = self.name, "the memory answer does not encode");
                return;
            }
        };
        match tokio::time::timeout(PUT_TIMEOUT, bucket.update(&key, bytes.into(), ticket.0)).await {
            Ok(Ok(revision)) => self.keep_entry(key, revision, Some(answer), Duration::ZERO),
            // A writer changed the key during the read, or another host put
            // it first. The watch brings the current value.
            Ok(Err(error))
                if error.kind()
                    == async_nats::jetstream::kv::UpdateErrorKind::WrongLastRevision => {}
            Ok(Err(error)) => {
                tracing::warn!(%error, bucket = self.name, key, "the memory answer was not put");
            }
            Err(_) => tracing::warn!(bucket = self.name, key, "the memory answer put timed out"),
        }
    }

    /// Keep what the watch of the bucket delivered.
    pub(crate) fn apply(&self, entry: KvEntry) {
        let answer = match entry.operation {
            Operation::Put => match serde_json::from_slice(&entry.value) {
                Ok(answer) => Some(answer),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        bucket = self.name,
                        key = entry.key,
                        "the memory answer does not decode"
                    );
                    None
                }
            },
            Operation::Delete | Operation::Purge => None,
        };
        let age = SystemTime::now()
            .duration_since(SystemTime::from(entry.created))
            .unwrap_or_default();
        self.keep_entry(entry.key, entry.revision, answer, age);
    }
}

/// A memory that the watch of one bucket fills.
pub(crate) trait Watched: Send + Sync {
    /// Hold `bucket` while the watch runs, and `None` after it breaks.
    fn set_bucket(&self, bucket: Option<Store>);
    /// Drop every fact.
    fn clear(&self);
    /// Keep one entry the watch delivered.
    fn apply(&self, entry: KvEntry);
}

/// Watch `bucket` through `filters` into `memory`. The watch runs until the client closes, and opens the bucket
/// again after it breaks.
pub(crate) fn spawn_watch(
    client: &async_nats::Client,
    bucket: &'static str,
    filters: Vec<String>,
    memory: Arc<dyn Watched>,
) {
    let client = client.clone();
    tokio::spawn(async move {
        loop {
            if let Err(error) = watch(&client, bucket, &filters, memory.as_ref()).await {
                tracing::warn!(
                    error = format!("{error:#}"),
                    bucket,
                    "the memory watch broke"
                );
            }
            // Without the watch, a delete can pass the host unseen.
            memory.set_bucket(None);
            memory.clear();
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}

/// The entries under `filter`, latest first, then each change.
///
/// A named consumer with one filter puts the filter in its create subject, so
/// the NATS permissions confine the watch to the keys of the host's
/// environment (docs/plan/alignment.md A7, R5). An unnamed watch of the client
/// library carries its filter only in the request body.
async fn filtered_watch(
    client: &async_nats::Client,
    store: &Store,
    filter: &str,
) -> anyhow::Result<impl futures_util::Stream<Item = anyhow::Result<KvEntry>> + Unpin + use<>> {
    use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy, push};
    let prefix = store.prefix.clone();
    let bucket = store.name.clone();
    let consumer = store
        .stream
        .create_consumer(push::Config {
            name: Some(format!("watch-{}", uuid::Uuid::new_v4().simple())),
            deliver_subject: client.new_inbox(),
            filter_subject: format!("{prefix}{filter}"),
            deliver_policy: DeliverPolicy::LastPerSubject,
            ack_policy: AckPolicy::None,
            memory_storage: true,
            inactive_threshold: Duration::from_secs(30),
            ..Default::default()
        })
        .await?;
    Ok(consumer.messages().await?.map(move |message| {
        let message = message?;
        let info = message.info().map_err(|error| anyhow::anyhow!("{error}"))?;
        let operation = match message
            .headers
            .as_ref()
            .and_then(|headers| headers.get("KV-Operation"))
        {
            Some(operation) => operation
                .as_str()
                .parse()
                .map_err(|error| anyhow::anyhow!("{error}"))?,
            None => Operation::Put,
        };
        Ok(KvEntry {
            bucket: bucket.clone(),
            key: message
                .subject
                .strip_prefix(prefix.as_str())
                .unwrap_or_default()
                .to_owned(),
            revision: info.stream_sequence,
            delta: info.pending,
            created: info.published,
            seen_current: info.pending == 0,
            value: message.message.payload.clone(),
            operation,
        })
    }))
}

async fn watch(
    client: &async_nats::Client,
    bucket: &str,
    filters: &[String],
    memory: &dyn Watched,
) -> anyhow::Result<()> {
    let store = wamn_broker::kv_memory::open_bucket(client, bucket).await?;
    let mut watches = Vec::with_capacity(filters.len());
    for filter in filters {
        watches.push(filtered_watch(client, &store, filter).await?);
    }
    let mut entries = futures_util::stream::select_all(watches);
    // Facts kept before the watch ran can miss a delete.
    memory.clear();
    memory.set_bucket(Some(store));
    // The client reconnects without ending the watch, so a lost connection
    // breaks the watch here.
    let mut connection = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            entry = entries.next() => match entry {
                Some(entry) => memory.apply(entry?),
                None => anyhow::bail!("the watch ended"),
            },
            _ = connection.tick() => {
                if client.connection_state() != async_nats::connection::State::Connected {
                    anyhow::bail!("the connection to the bucket is lost");
                }
            }
        }
    }
}

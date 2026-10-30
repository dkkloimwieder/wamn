//! The host's memory of the connection facts of a call
//! (docs/architecture/execution.md#connection-memory).
//!
//! Postgres stays the owner of every fact here. The memory is the local
//! replica of the NATS KV bucket [`BUCKET`] on the NATS of the auth memory,
//! under the rules of [`crate::kv_memory`]. The key
//! `<env_key>.connection.<connection id>` holds the active generation, the
//! effective status and the credential id of a connection. The key
//! `<env_key>.release.<manifest digest hex>` holds the connection map of a
//! release. The key `<env_key>.connections` holds the map from integration
//! name to connection id of the environment. Every writer of a fact deletes its key after the commit
//! ([`wamn_platform_identity::connection_changes`]).
//!
//! Immutable rows, such as the parameters of a generation or the secret of a
//! credential, stay in the host under their own ids and never enter the
//! bucket.
//!
//! [`BUCKET`]: wamn_platform_identity::connection_changes::BUCKET

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::kv::Store;
use wamn_platform_identity::connection_changes::{
    BUCKET, ConnectionChanges, connection_key, connections_key, release_key,
};
use wamn_platform_identity::kv_memory::MEMORY_TTL;

pub use crate::kv_memory::Ticket;
use crate::kv_memory::{KvMemory, Watched};

/// The effective status of a connection: active only when the integration
/// and the connection are both active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    Active,
    Disabled,
}

/// The mutable fact of one connection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionFact {
    pub active_generation: i64,
    pub status: ConnectionStatus,
    /// The credential of the active generation, when it has one.
    pub credential_id: Option<uuid::Uuid>,
}

/// One name a release binds: (component, package, name) to (connection,
/// version, mode).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseBinding {
    pub component_digest: String,
    pub package_id: String,
    pub name: String,
    pub connection_id: uuid::Uuid,
    pub version: i32,
    pub mode: String,
}

/// The connection map of one release.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseFact {
    pub versions: BTreeMap<uuid::Uuid, i32>,
    pub bindings: Vec<ReleaseBinding>,
}

impl ReleaseFact {
    /// The released version of a connection, including one with no binding.
    pub fn version(&self, connection_id: &uuid::Uuid) -> Option<i32> {
        self.versions.get(connection_id).copied()
    }

    /// The binding of `name` for one component of one package.
    pub fn binding(
        &self,
        component_digest: &str,
        package_id: &str,
        name: &str,
    ) -> Option<&ReleaseBinding> {
        self.bindings.iter().find(|binding| {
            binding.component_digest == component_digest
                && binding.package_id == package_id
                && binding.name == name
        })
    }
}

/// The map from integration name to connection id of one environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvConnections {
    pub connections: BTreeMap<String, uuid::Uuid>,
}

/// The value of a key of the bucket.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Answer {
    Connection(ConnectionFact),
    Release(ReleaseFact),
    Connections(EnvConnections),
}

/// The connection facts one host keeps.
pub struct ConnectionMemory {
    memory: KvMemory<Answer>,
}

impl Default for ConnectionMemory {
    fn default() -> Self {
        Self::with_ttl(MEMORY_TTL)
    }
}

impl std::fmt::Debug for ConnectionMemory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionMemory")
            .field("ttl", &self.memory.ttl())
            .finish_non_exhaustive()
    }
}

impl ConnectionMemory {
    /// A memory whose facts live for `ttl`.
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            memory: KvMemory::new(BUCKET, ttl),
        }
    }

    /// Whether the host watches the bucket, and so keeps facts.
    pub fn watched(&self) -> bool {
        self.memory.bucket().is_some()
    }

    /// The fact of connection `id`, or the ticket of a read of it.
    pub fn connection(&self, env_key: &str, id: &uuid::Uuid) -> Result<ConnectionFact, Ticket> {
        let key = connection_key(env_key, id);
        match self.memory.answer(&key) {
            Ok(Answer::Connection(fact)) => Ok(fact),
            Ok(_) => Err(self.memory.ticket(&key)),
            Err(ticket) => Err(ticket),
        }
    }

    pub async fn keep_connection(
        &self,
        env_key: &str,
        id: &uuid::Uuid,
        ticket: Ticket,
        fact: ConnectionFact,
    ) {
        self.memory
            .put(
                connection_key(env_key, id),
                ticket,
                Answer::Connection(fact),
            )
            .await;
    }

    /// The connection map of the release `manifest_digest`, or the ticket of
    /// a read of it.
    pub fn release(&self, env_key: &str, manifest_digest: &str) -> Result<ReleaseFact, Ticket> {
        let key = release_key(env_key, manifest_digest);
        match self.memory.answer(&key) {
            Ok(Answer::Release(fact)) => Ok(fact),
            Ok(_) => Err(self.memory.ticket(&key)),
            Err(ticket) => Err(ticket),
        }
    }

    pub async fn keep_release(
        &self,
        env_key: &str,
        manifest_digest: &str,
        ticket: Ticket,
        fact: ReleaseFact,
    ) {
        self.memory
            .put(
                release_key(env_key, manifest_digest),
                ticket,
                Answer::Release(fact),
            )
            .await;
    }
}

impl ConnectionMemory {
    /// The connection map of `env_key`, or the ticket of a read of it.
    pub fn connections(&self, env_key: &str) -> Result<EnvConnections, Ticket> {
        let key = connections_key(env_key);
        match self.memory.answer(&key) {
            Ok(Answer::Connections(map)) => Ok(map),
            Ok(_) => Err(self.memory.ticket(&key)),
            Err(ticket) => Err(ticket),
        }
    }

    pub async fn keep_connections(&self, env_key: &str, ticket: Ticket, map: EnvConnections) {
        self.memory
            .put(connections_key(env_key), ticket, Answer::Connections(map))
            .await;
    }
}

impl Watched for ConnectionMemory {
    fn set_bucket(&self, bucket: Option<Store>) {
        self.memory.set_bucket(bucket);
    }

    fn clear(&self) {
        self.memory.clear();
    }

    fn apply(&self, entry: async_nats::jetstream::kv::Entry) {
        self.memory.apply(entry);
    }
}

impl AsRef<ConnectionMemory> for ConnectionMemory {
    fn as_ref(&self) -> &ConnectionMemory {
        self
    }
}

/// Watch the bucket into the memory that `holder` holds, and return the
/// writer of the host's own changes. A host with `env_key` watches the keys
/// of that `env_key`. The control host serves every tenant, so it watches
/// every key.
pub fn replicate<H: AsRef<ConnectionMemory> + Send + Sync + 'static>(
    client: &async_nats::Client,
    env_key: Option<&str>,
    holder: Arc<H>,
) -> ConnectionChanges {
    let filters = match env_key {
        Some(env_key) => vec![format!("{env_key}.>")],
        None => vec![">".to_owned()],
    };
    crate::kv_memory::spawn_watch(client, BUCKET, filters, holder, |holder| {
        (*holder).as_ref() as &dyn Watched
    });
    ConnectionChanges::new(client.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_cache::test_util::{nats, until};
    use wamn_platform_identity::connection_changes::ConnectionDrops;

    fn fact(generation: i64) -> ConnectionFact {
        ConnectionFact {
            active_generation: generation,
            status: ConnectionStatus::Active,
            credential_id: Some(uuid::Uuid::new_v4()),
        }
    }

    async fn watched(
        client: &async_nats::Client,
        env_key: Option<&str>,
    ) -> (Arc<ConnectionMemory>, ConnectionChanges) {
        let memory = Arc::new(ConnectionMemory::default());
        let writer = replicate(client, env_key, Arc::clone(&memory));
        until("the watch runs", || memory.watched()).await;
        (memory, writer)
    }

    #[test]
    fn a_release_version_does_not_require_a_binding() {
        let id = uuid::Uuid::new_v4();
        let fact = ReleaseFact {
            versions: BTreeMap::from([(id, 7)]),
            bindings: Vec::new(),
        };
        assert_eq!(fact.version(&id), Some(7));
        assert_eq!(fact.version(&uuid::Uuid::new_v4()), None);
        assert!(fact.bindings.is_empty());
    }

    #[tokio::test]
    async fn a_host_without_the_bucket_keeps_no_fact() {
        let memory = ConnectionMemory::default();
        let id = uuid::Uuid::new_v4();
        let ticket = memory.connection("t", &id).unwrap_err();
        memory.keep_connection("t", &id, ticket, fact(1)).await;
        let ticket = memory.release("t", "sha256:00").unwrap_err();
        memory
            .keep_release("t", "sha256:00", ticket, ReleaseFact::default())
            .await;
        assert!(memory.connection("t", &id).is_err());
        assert!(memory.release("t", "sha256:00").is_err());
    }

    #[tokio::test]
    #[ignore = "requires: WAMN_NATIVE_C_NATS_BIN"]
    async fn a_delete_during_a_read_refuses_the_put() {
        let (_server, client) = nats().await;
        let (memory, writer) = watched(&client, Some("t")).await;
        let id = uuid::Uuid::new_v4();
        let ticket = memory.connection("t", &id).unwrap_err();
        let mut drops = ConnectionDrops::default();
        drops.connection("t", id);
        writer.publish(drops).await;
        memory.keep_connection("t", &id, ticket, fact(1)).await;
        assert!(memory.connection("t", &id).is_err());
        // A read after the delete keeps its fact.
        until("the host sees the delete", || {
            memory.memory.ticket(&connection_key("t", &id)) != ticket
        })
        .await;
        let second = fact(2);
        let ticket = memory.connection("t", &id).unwrap_err();
        memory
            .keep_connection("t", &id, ticket, second.clone())
            .await;
        assert_eq!(memory.connection("t", &id), Ok(second));
    }

    #[tokio::test]
    #[ignore = "requires: WAMN_NATIVE_C_NATS_BIN"]
    async fn facts_reach_the_hosts_of_their_env_key_and_every_delete_reaches_them() {
        let (_server, client) = nats().await;
        let (a, writer) = watched(&client, Some("t")).await;
        let (b, _) = watched(&client, Some("t")).await;
        let (other, _) = watched(&client, Some("u")).await;
        let (control, _) = watched(&client, None).await;
        let id = uuid::Uuid::new_v4();
        let digest = format!("sha256:{}", "ab".repeat(32));
        let release = ReleaseFact {
            versions: BTreeMap::from([(id, 1)]),
            bindings: vec![ReleaseBinding {
                component_digest: format!("sha256:{}", "cd".repeat(32)),
                package_id: "shop".into(),
                name: "erp".into(),
                connection_id: id,
                version: 1,
                mode: "write".into(),
            }],
        };
        let first = fact(1);
        let ticket = a.connection("t", &id).unwrap_err();
        a.keep_connection("t", &id, ticket, first.clone()).await;
        let ticket = a.release("t", &digest).unwrap_err();
        a.keep_release("t", &digest, ticket, release.clone()).await;
        assert_eq!(a.connection("t", &id), Ok(first.clone()));
        until("host b sees the facts", || {
            b.connection("t", &id).is_ok() && b.release("t", &digest).is_ok()
        })
        .await;
        until("the control host sees the facts", || {
            control.connection("t", &id).is_ok()
        })
        .await;
        assert!(other.connection("t", &id).is_err());
        assert_eq!(
            b.release("t", &digest).unwrap().binding(
                &release.bindings[0].component_digest,
                "shop",
                "erp"
            ),
            Some(&release.bindings[0])
        );

        // A connection delete leaves the release map.
        let mut drops = ConnectionDrops::default();
        drops.connection("t", id);
        writer.publish(drops).await;
        until("hosts a and b drop the connection", || {
            a.connection("t", &id).is_err() && b.connection("t", &id).is_err()
        })
        .await;
        assert!(b.release("t", &digest).is_ok());

        // An environment delete drops every key of the environment.
        let ticket = a.connection("t", &id).unwrap_err();
        a.keep_connection("t", &id, ticket, first.clone()).await;
        let ticket = other.connection("u", &id).unwrap_err();
        other.keep_connection("u", &id, ticket, first.clone()).await;
        until("host b sees the fact again", || {
            b.connection("t", &id).is_ok()
        })
        .await;
        let mut drops = ConnectionDrops::default();
        drops.env_key("t");
        writer.publish(drops).await;
        until("host b drops the environment", || {
            b.connection("t", &id).is_err() && b.release("t", &digest).is_err()
        })
        .await;
        assert_eq!(other.connection("u", &id), Ok(first));
    }
}

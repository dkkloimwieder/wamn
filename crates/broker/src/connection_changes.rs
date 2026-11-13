//! The connection facts a committed write changed, and their deletion from
//! the memory of every host (docs/architecture/execution.md#connection-memory).
//!
//! Every writer of a fact a host keeps records it in a [`ConnectionDrops`]
//! in its transaction. Its caller deletes the keys with
//! [`crate::Publisher::publish`] after the commit. The facts are the active
//! generation, effective status and credential of a connection, the
//! connection map of a release, and the connection map of an environment.

use std::collections::BTreeSet;

use crate::kv_memory::Deletion;

/// The NATS KV bucket of the connection memory, on the NATS of the auth
/// memory.
pub const BUCKET: &str = "wamn-connection";

/// The key of the fact of connection `id` in `env_key`: its active
/// generation, effective status and credential id.
pub fn connection_key(env_key: &str, id: &uuid::Uuid) -> String {
    format!("{env_key}.connection.{id}")
}

/// The key of the connection map of the release `manifest_digest` in
/// `env_key`. The key carries the hex part of the digest only, because a key
/// token holds no `:`.
pub fn release_key(env_key: &str, manifest_digest: &str) -> String {
    let hex = manifest_digest
        .strip_prefix("sha256:")
        .unwrap_or(manifest_digest);
    format!("{env_key}.release.{hex}")
}

/// The key of the map from integration name to connection id of `env_key`.
pub fn connections_key(env_key: &str) -> String {
    format!("{env_key}.connections")
}

/// One connection fact that a committed write changed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConnectionChange {
    /// The fact of one connection.
    Connection { env_key: String, id: uuid::Uuid },
    /// The connection map of one release.
    Release {
        env_key: String,
        manifest_digest: String,
    },
    /// The map from integration name to connection id of an environment.
    Connections(String),
    /// Every fact of an environment.
    EnvKey(String),
}

/// The changes of one transaction, deleted after its commit.
#[must_use = "delete the drops with Publisher::publish after the commit"]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionDrops(BTreeSet<ConnectionChange>);

impl ConnectionDrops {
    pub fn connection(&mut self, env_key: &str, id: uuid::Uuid) {
        self.0.insert(ConnectionChange::Connection {
            env_key: env_key.to_owned(),
            id,
        });
    }

    pub fn release(&mut self, env_key: &str, manifest_digest: &str) {
        self.0.insert(ConnectionChange::Release {
            env_key: env_key.to_owned(),
            manifest_digest: manifest_digest.to_owned(),
        });
    }

    pub fn connections(&mut self, env_key: &str) {
        self.0
            .insert(ConnectionChange::Connections(env_key.to_owned()));
    }

    pub fn env_key(&mut self, env_key: &str) {
        self.0.insert(ConnectionChange::EnvKey(env_key.to_owned()));
    }

    /// Add the changes of `other`, for a caller that runs several writers in
    /// one transaction.
    pub fn extend(&mut self, other: ConnectionDrops) {
        self.0.extend(other.0);
    }

    pub fn changes(&self) -> impl Iterator<Item = &ConnectionChange> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl crate::Changes for ConnectionDrops {
    fn into_deletions(self) -> (&'static str, Vec<Deletion>) {
        (
            BUCKET,
            self.0
                .into_iter()
                .map(|change| match change {
                    ConnectionChange::Connection { env_key, id } => {
                        Deletion::Key(connection_key(&env_key, &id))
                    }
                    ConnectionChange::Release {
                        env_key,
                        manifest_digest,
                    } => Deletion::Key(release_key(&env_key, &manifest_digest)),
                    ConnectionChange::Connections(env_key) => {
                        Deletion::Key(connections_key(&env_key))
                    }
                    ConnectionChange::EnvKey(env_key) => Deletion::Under(env_key),
                })
                .collect(),
        )
    }
}

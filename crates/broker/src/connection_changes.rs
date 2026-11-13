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

/// The key of the fact of connection `id` in `project_env_key`: its active
/// generation, effective status and credential id.
pub fn connection_key(project_env_key: &str, id: &uuid::Uuid) -> String {
    wamn_registry::name::connection_key(project_env_key, id)
}

/// The key of the connection map of the release `manifest_digest` in
/// `project_env_key`. The key carries the hex part of the digest only, because a key
/// token holds no `:`.
pub fn release_key(project_env_key: &str, manifest_digest: &str) -> String {
    let hex = manifest_digest
        .strip_prefix("sha256:")
        .unwrap_or(manifest_digest);
    wamn_registry::name::nats_subject(&[project_env_key, "release", hex])
}

/// The key of the map from integration name to connection id of `project_env_key`.
pub fn connections_key(project_env_key: &str) -> String {
    wamn_registry::name::nats_subject(&[project_env_key, "connections"])
}

/// One connection fact that a committed write changed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConnectionChange {
    /// The fact of one connection.
    Connection {
        project_env_key: String,
        id: uuid::Uuid,
    },
    /// The connection map of one release.
    Release {
        project_env_key: String,
        manifest_digest: String,
    },
    /// The map from integration name to connection id of an environment.
    Connections(String),
    /// Every fact of an environment.
    ProjectEnvKey(String),
}

/// The changes of one transaction, deleted after its commit.
#[must_use = "delete the drops with Publisher::publish after the commit"]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionDrops(BTreeSet<ConnectionChange>);

impl ConnectionDrops {
    pub fn connection(&mut self, project_env_key: &str, id: uuid::Uuid) {
        self.0.insert(ConnectionChange::Connection {
            project_env_key: project_env_key.to_owned(),
            id,
        });
    }

    pub fn release(&mut self, project_env_key: &str, manifest_digest: &str) {
        self.0.insert(ConnectionChange::Release {
            project_env_key: project_env_key.to_owned(),
            manifest_digest: manifest_digest.to_owned(),
        });
    }

    pub fn connections(&mut self, project_env_key: &str) {
        self.0
            .insert(ConnectionChange::Connections(project_env_key.to_owned()));
    }

    pub fn project_env_key(&mut self, project_env_key: &str) {
        self.0
            .insert(ConnectionChange::ProjectEnvKey(project_env_key.to_owned()));
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
                    ConnectionChange::Connection {
                        project_env_key,
                        id,
                    } => Deletion::Key(connection_key(&project_env_key, &id)),
                    ConnectionChange::Release {
                        project_env_key,
                        manifest_digest,
                    } => Deletion::Key(release_key(&project_env_key, &manifest_digest)),
                    ConnectionChange::Connections(project_env_key) => {
                        Deletion::Key(connections_key(&project_env_key))
                    }
                    ConnectionChange::ProjectEnvKey(project_env_key) => {
                        Deletion::Under(project_env_key)
                    }
                })
                .collect(),
        )
    }
}

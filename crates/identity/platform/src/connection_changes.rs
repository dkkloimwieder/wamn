//! The connection facts a committed write changed, and their deletion from
//! the memory of every host (docs/architecture/execution.md#connection-memory).
//!
//! Every writer of a fact a host keeps records it in a [`ConnectionDrops`]
//! in its transaction. Its caller deletes the keys with
//! [`ConnectionChanges::publish`] after the commit. The facts are the active
//! generation, effective status and credential of a connection, and the
//! connection map of a release.

use std::collections::BTreeSet;

use crate::auth_changes::{AuthChanges, AuthNats};
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
    /// Every fact of an environment.
    EnvKey(String),
}

/// The changes of one transaction, deleted after its commit.
#[must_use = "delete the drops with ConnectionChanges::publish after the commit"]
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

/// Deletes the keys of the changes of a commit from the bucket. A writer
/// without a NATS client deletes nothing, and the TTL of the memory applies.
#[derive(Debug, Clone, Default)]
pub struct ConnectionChanges(Option<async_nats::Client>);

impl ConnectionChanges {
    pub fn new(client: async_nats::Client) -> Self {
        Self(Some(client))
    }

    /// Connect to the NATS that `connection` names.
    pub async fn connect(connection: &AuthNats) -> anyhow::Result<Self> {
        Ok(Self(
            AuthChanges::connect(connection).await?.client().cloned(),
        ))
    }

    /// Delete the keys of `drops` after the commit that made them. A failed
    /// delete is logged, because the commit stands and the TTL bounds it.
    pub async fn publish(&self, drops: ConnectionDrops) {
        let Some(client) = &self.0 else {
            return;
        };
        let deletions = drops
            .0
            .into_iter()
            .map(|change| match change {
                ConnectionChange::Connection { env_key, id } => {
                    Deletion::Key(connection_key(&env_key, &id))
                }
                ConnectionChange::Release {
                    env_key,
                    manifest_digest,
                } => Deletion::Key(release_key(&env_key, &manifest_digest)),
                ConnectionChange::EnvKey(env_key) => Deletion::Under(env_key),
            })
            .collect();
        crate::kv_memory::delete(client, BUCKET, deletions).await;
    }
}

impl From<&AuthChanges> for ConnectionChanges {
    /// The writer of the connection memory on the client of the auth memory.
    fn from(auth: &AuthChanges) -> Self {
        Self(auth.client().cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_carry_the_env_key_the_fact_type_and_its_id() {
        let id = uuid::Uuid::nil();
        assert_eq!(
            connection_key("acme--shop--dev", &id),
            "acme--shop--dev.connection.00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(
            release_key("acme--shop--dev", &format!("sha256:{}", "ab".repeat(32))),
            format!("acme--shop--dev.release.{}", "ab".repeat(32))
        );
    }
}

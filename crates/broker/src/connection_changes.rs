use std::collections::BTreeSet;

/// The NATS KV bucket of the connection memory, on the NATS of the auth
/// memory.
pub const BUCKET: &str = "wamn-connection";

/// The key of the fact of connection `id` in `project_env_key`: its active
/// generation, effective status and credential id.
pub fn connection_key(project_env_key: &wamn_registry::ProjectEnvKey, id: &uuid::Uuid) -> String {
    wamn_registry::name::connection_key(project_env_key, id)
}

/// The key of the connection map of the release `manifest_digest` in
/// `project_env_key`. The key carries the hex part of the digest only, because a key
/// token holds no `:`.
pub fn release_key(
    project_env_key: &wamn_registry::ProjectEnvKey,
    manifest_digest: &wamn_digest::Digest,
) -> String {
    let hex = manifest_digest.hex();
    wamn_registry::name::nats_subject(&[project_env_key.as_str(), "release", &hex])
}

/// The key of the map from integration name to connection id of `project_env_key`.
pub fn connections_key(project_env_key: &wamn_registry::ProjectEnvKey) -> String {
    wamn_registry::name::nats_subject(&[project_env_key.as_str(), "connections"])
}

/// One connection fact that a committed write changed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConnectionChange {
    /// The fact of one connection.
    Connection {
        project_env_key: wamn_registry::ProjectEnvKey,
        id: uuid::Uuid,
    },
    /// The connection map of one release.
    Release {
        project_env_key: wamn_registry::ProjectEnvKey,
        manifest_digest: wamn_digest::Digest,
    },
    /// The map from integration name to connection id of an environment.
    Connections(wamn_registry::ProjectEnvKey),
    /// Every fact of an environment.
    ProjectEnvKey(wamn_registry::ProjectEnvKey),
}

/// The changes of one transaction, deleted after its commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionDrops(BTreeSet<ConnectionChange>);

impl ConnectionDrops {
    pub fn connection(&mut self, _project_env_key: &wamn_registry::ProjectEnvKey, _id: uuid::Uuid) {
    }

    pub fn release(
        &mut self,
        _project_env_key: &wamn_registry::ProjectEnvKey,
        _manifest_digest: &wamn_digest::Digest,
    ) {
    }

    pub fn connections(&mut self, _project_env_key: &wamn_registry::ProjectEnvKey) {}

    pub fn project_env_key(&mut self, _project_env_key: &wamn_registry::ProjectEnvKey) {}

    /// Add the changes of `other`, for a caller that runs several writers in
    /// one transaction.
    pub fn extend(&mut self, _other: ConnectionDrops) {}

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl crate::Changes for ConnectionDrops {}

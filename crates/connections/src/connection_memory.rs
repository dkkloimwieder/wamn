//! The host's memory of the connection facts of a call
//! (docs/architecture/execution.md#connection-memory).
//!

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use wamn_broker::Publisher;

use wamn_broker::kv_memory::CONNECTION_MEMORY_TTL;

pub use wamn_authority::kv_memory::Ticket;

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
    pub service_principal_id: wamn_identity_protocol::PrincipalId,
    pub exclusive: bool,
    pub target_id: uuid::Uuid,
    pub resource_key: Option<String>,
    pub target_type: String,
}

/// One name a release binds: (component, package, name) to (connection,
/// version).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseBinding {
    pub component_digest: wamn_digest::Digest,
    pub package_id: wamn_catalog::PackageName,
    pub name: String,
    pub connection_id: uuid::Uuid,
    pub version: i32,
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
        component_digest: &wamn_digest::Digest,
        package_id: &wamn_catalog::PackageName,
        name: &str,
    ) -> Option<&ReleaseBinding> {
        self.bindings.iter().find(|binding| {
            binding.component_digest == *component_digest
                && &binding.package_id == package_id
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

/// The connection facts one host keeps.
pub struct ConnectionMemory {
    pub oauth_tokens: Arc<crate::oauth_token::Cache>,
    pub resource_leases: Arc<crate::resource_lease::ResourceLeases>,
}

impl Default for ConnectionMemory {
    fn default() -> Self {
        Self::with_ttl(CONNECTION_MEMORY_TTL)
    }
}

impl std::fmt::Debug for ConnectionMemory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionMemory")
            .finish_non_exhaustive()
    }
}

impl ConnectionMemory {
    /// A memory whose facts live for `ttl`.
    pub fn with_ttl(_ttl: Duration) -> Self {
        Self {
            oauth_tokens: Arc::default(),
            resource_leases: Arc::new(
                crate::resource_lease::ResourceLeases::new(None).expect("no lease database URL"),
            ),
        }
    }

    /// Give this host's leases the same login as its connection reader.
    pub fn with_resource_leases(
        mut self,
        leases: Arc<crate::resource_lease::ResourceLeases>,
    ) -> Self {
        self.resource_leases = leases;
        self
    }

    /// Whether the host watches the bucket, and so keeps facts.
    pub fn watched(&self) -> bool {
        false
    }

    /// The fact of connection `id`, or the ticket of a read of it.
    pub fn connection(
        &self,
        _project_env_key: &wamn_registry::ProjectEnvKey,
        _id: &uuid::Uuid,
    ) -> Result<ConnectionFact, Ticket> {
        Err(Ticket(0))
    }

    pub async fn keep_connection(
        &self,
        _project_env_key: &wamn_registry::ProjectEnvKey,
        _id: &uuid::Uuid,
        _ticket: Ticket,
        _fact: ConnectionFact,
    ) {
    }
}

pub fn replicate(
    _client: &async_nats::Client,
    _project_env_key: Option<&wamn_registry::ProjectEnvKey>,
    _holder: Arc<ConnectionMemory>,
) -> Publisher {
    Publisher::default()
}

#[cfg(test)]
mod tests {
    use super::*;
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
}

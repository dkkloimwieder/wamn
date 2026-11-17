//! The host's memory of the connection facts of a call
//! (docs/architecture/execution.md#connection-memory).
//!

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

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

/// The map from integration name to connection id of one environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvConnections {
    pub connections: BTreeMap<String, uuid::Uuid>,
}

/// The connection facts one host keeps.
pub struct ConnectionMemory {
    pub oauth_tokens: Arc<crate::oauth_token::Cache>,
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
        }
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

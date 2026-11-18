//! The host's memory of the connection facts of a call
//! (docs/architecture/execution.md#connection-memory).
//!

use std::collections::BTreeMap;
use std::sync::Arc;

use moka::policy::EvictionPolicy;
use moka::sync::Cache;

/// The number of entries of each bounded map.
const MAP_ENTRIES: u64 = 1024;

/// A bounded map that evicts its least recently used entry.
pub fn bounded<K, V>() -> Cache<K, V>
where
    K: std::hash::Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    Cache::builder()
        .max_capacity(MAP_ENTRIES)
        .eviction_policy(EvictionPolicy::lru())
        .build()
}

/// The status of the project database's platform.connection row.
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
    pub target_id: uuid::Uuid,
    pub resource_key: Option<String>,
    pub authority: Option<String>,
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
        Self {
            oauth_tokens: Arc::default(),
        }
    }
}

impl std::fmt::Debug for ConnectionMemory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionMemory")
            .finish_non_exhaustive()
    }
}

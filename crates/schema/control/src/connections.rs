//! Portable connection requirements and environment-owned persistence records.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use wamn_catalog::ConnectionTypeDescriptor;

/// Controlled lifecycle states for an environment-owned connection instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionInstanceStatus {
    Enabled,
    Disabled,
}

impl ConnectionInstanceStatus {
    /// Return the database literal pinned by the catalog schema.
    pub fn as_sql(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
        }
    }
}

/// A portable component-owned connection requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentConnectionRequirement {
    component_digest: String,
    store_alias: String,
    requirement: ConnectionTypeDescriptor,
}

impl ComponentConnectionRequirement {
    /// Construct one requirement from its admitted component identity.
    pub fn new(
        component_digest: impl Into<String>,
        store_alias: impl Into<String>,
        requirement: ConnectionTypeDescriptor,
    ) -> Self {
        Self {
            component_digest: component_digest.into(),
            store_alias: store_alias.into(),
            requirement,
        }
    }

    pub fn component_digest(&self) -> &str {
        &self.component_digest
    }

    pub fn store_alias(&self) -> &str {
        &self.store_alias
    }

    pub fn requirement(&self) -> &ConnectionTypeDescriptor {
        &self.requirement
    }

    /// Canonical environment-independent bytes persisted with the component.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("portable component connection requirement serializes")
    }

    /// SHA-256 of [`Self::canonical_bytes`].
    pub fn requirement_hash(&self) -> String {
        prefixed_sha256(&self.canonical_bytes())
    }
}

/// The non-secret definition of one immutable environment generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ConnectionGenerationDefinition {
    pub primary_authority: String,
    pub failover_authorities: Vec<String>,
    pub tls_policy: String,
    pub redirect_policy: String,
    pub proxy_reference: Option<String>,
}

impl ConnectionGenerationDefinition {
    /// Stable hash stored beside the immutable definition.
    pub fn definition_hash(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("connection generation definition serializes");
        prefixed_sha256(&bytes)
    }
}

/// An environment-owned stable connection identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInstance {
    pub tenant_id: String,
    pub environment: String,
    pub instance_id: String,
    pub requirement_type: String,
    pub contract: String,
    pub status: ConnectionInstanceStatus,
    pub active_generation: Option<i64>,
    pub revision: i64,
}

/// One immutable, non-secret connection definition generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionGeneration {
    pub tenant_id: String,
    pub environment: String,
    pub instance_id: String,
    pub generation: i64,
    pub definition: ConnectionGenerationDefinition,
    pub credential_set_handle: String,
}

/// An immutable component-release association to one stable instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentConnectionBinding {
    pub tenant_id: String,
    pub effective_release_id: i32,
    pub component_digest: String,
    pub store_alias: String,
    pub environment: String,
    pub instance_id: String,
    pub status: ConnectionBindingStatus,
    pub validation: ConnectionBindingValidation,
    pub validation_hash: String,
}

/// Controlled state of an immutable release binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionBindingStatus {
    Active,
    Disabled,
}

/// Persisted outcome of binding compatibility validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionBindingValidation {
    Valid,
    Invalid,
}

/// Insert one immutable component requirement; identical retries converge.
pub fn insert_component_connection_requirement_sql() -> &'static str {
    "INSERT INTO catalog.connection_requirements \
       (tenant_id, component_digest, store_alias, requirement_json, requirement_hash) \
     VALUES ($1, $2, $3, $4::text::jsonb, $5) \
     ON CONFLICT DO NOTHING"
}

/// Prove an existing component requirement row is byte-identical to this one.
///
/// The parameters are exactly [`insert_component_connection_requirement_sql`]'s,
/// so a writer whose insert converged away can tell whether it converged onto
/// its own record or collided with a different one at the same coordinate.
pub fn exact_component_connection_requirement_sql() -> &'static str {
    "SELECT EXISTS (\
       SELECT 1 FROM catalog.connection_requirements \
        WHERE tenant_id = $1 AND component_digest = $2 AND store_alias = $3 \
          AND requirement_json = $4::text::jsonb AND requirement_hash = $5\
     )"
}

/// Insert one environment-owned stable instance identity.
pub fn insert_connection_instance_sql() -> &'static str {
    "INSERT INTO catalog.connection_instances \
       (tenant_id, environment, instance_id, requirement_type, contract) \
     VALUES ($1, $2, $3, $4, $5)"
}

/// Insert one immutable generation; secret material is represented only by a handle.
pub fn insert_connection_generation_sql() -> &'static str {
    "INSERT INTO catalog.connection_generations \
       (tenant_id, environment, instance_id, generation, definition_json, \
        definition_hash, credential_set_handle) \
     VALUES ($1, $2, $3, $4, $5::text::jsonb, $6, $7)"
}

/// Activate one generation on its instance. The instance-update guard requires
/// every update to advance `revision`, so activation is a revision, not an
/// edit: the row's identity columns are immutable and the trigger refuses a
/// stale revision with `connection-instance-revision-must-advance`.
pub fn activate_connection_generation_sql() -> &'static str {
    "UPDATE catalog.connection_instances \
        SET active_generation = $4, revision = revision + 1 \
      WHERE tenant_id = $1 AND environment = $2 AND instance_id = $3"
}

/// Insert one immutable component release binding to an environment instance.
pub fn insert_component_connection_binding_sql() -> &'static str {
    "INSERT INTO catalog.connection_bindings \
       (tenant_id, effective_release_id, component_digest, store_alias, \
        environment, instance_id, binding_status, validation_status, validation_hash) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
}

fn prefixed_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

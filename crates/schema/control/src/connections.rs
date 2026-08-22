//! Portable connection requirements and environment-owned persistence records.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use wamn_schema_model::ConnectionTypeDescriptor;

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

/// A portable legacy flow-artifact-owned connection requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ArtifactConnectionRequirement {
    artifact_hash: String,
    requirement_name: String,
    requirement: ConnectionTypeDescriptor,
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
        hex_sha256(&self.canonical_bytes())
    }
}

impl ArtifactConnectionRequirement {
    /// Construct an immutable portable requirement record.
    pub fn new(
        artifact_hash: impl Into<String>,
        requirement_name: impl Into<String>,
        requirement: ConnectionTypeDescriptor,
    ) -> Self {
        Self {
            artifact_hash: artifact_hash.into(),
            requirement_name: requirement_name.into(),
            requirement,
        }
    }

    pub fn artifact_hash(&self) -> &str {
        &self.artifact_hash
    }

    pub fn requirement_name(&self) -> &str {
        &self.requirement_name
    }

    pub fn requirement(&self) -> &ConnectionTypeDescriptor {
        &self.requirement
    }

    /// Canonical environment-independent bytes persisted with the artifact.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("portable connection requirement serializes")
    }

    /// SHA-256 of [`Self::canonical_bytes`].
    pub fn requirement_hash(&self) -> String {
        hex_sha256(&self.canonical_bytes())
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
        hex_sha256(&bytes)
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

/// An immutable legacy flow-release association to one stable instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionBinding {
    pub tenant_id: String,
    pub catalog_id: String,
    pub catalog_version: i32,
    pub artifact_hash: String,
    pub requirement_name: String,
    pub environment: String,
    pub instance_id: String,
    pub status: ConnectionBindingStatus,
    pub validation: ConnectionBindingValidation,
    pub validation_hash: String,
}

/// An immutable component-release association to one stable instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentConnectionBinding {
    pub tenant_id: String,
    pub catalog_id: String,
    pub catalog_version: i32,
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

/// What keeps an immutable generation definition available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GenerationRetentionKind {
    ActiveAttempt,
    DeployedRelease,
}

impl GenerationRetentionKind {
    pub fn as_sql(self) -> &'static str {
        match self {
            Self::ActiveAttempt => "active-attempt",
            Self::DeployedRelease => "deployed-release",
        }
    }
}

/// Insert one immutable legacy artifact requirement; identical retries converge.
pub fn insert_connection_requirement_sql() -> &'static str {
    "INSERT INTO catalog.connection_requirements \
       (tenant_id, artifact_hash, requirement_name, requirement_json, requirement_hash) \
     VALUES ($1, $2, $3, $4::text::jsonb, $5) \
     ON CONFLICT DO NOTHING"
}

/// Insert one immutable component requirement; identical retries converge.
pub fn insert_component_connection_requirement_sql() -> &'static str {
    "INSERT INTO catalog.connection_requirements \
       (tenant_id, component_digest, store_alias, requirement_json, requirement_hash) \
     VALUES ($1, $2, $3, $4::text::jsonb, $5) \
     ON CONFLICT DO NOTHING"
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

/// Insert one immutable legacy release binding to an environment instance.
pub fn insert_connection_binding_sql() -> &'static str {
    "INSERT INTO catalog.connection_bindings \
       (tenant_id, catalog_id, catalog_version, artifact_hash, requirement_name, \
        environment, instance_id, binding_status, validation_status, validation_hash) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
}

/// Insert one immutable component release binding to an environment instance.
pub fn insert_component_connection_binding_sql() -> &'static str {
    "INSERT INTO catalog.connection_bindings \
       (tenant_id, catalog_id, catalog_version, component_digest, store_alias, \
        environment, instance_id, binding_status, validation_status, validation_hash) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
}

/// Retain a generation for one active attempt or deployed release.
pub fn insert_generation_retention_sql() -> &'static str {
    "INSERT INTO catalog.connection_generation_retention \
       (tenant_id, environment, instance_id, generation, reference_kind, \
        reference_id, retained_until) \
     VALUES ($1, $2, $3, $4, $5, $6, $7)"
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

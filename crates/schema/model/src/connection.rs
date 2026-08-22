//! Portable connection-type semantics shared by components and legacy flows.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Shape version for portable connection-type descriptors.
pub const CONNECTION_DESCRIPTOR_VERSION: &str = "1";

/// A field whose ownership is fixed by a connection-type descriptor.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionField {
    Method,
    RelativeTarget,
    Headers,
    Body,
    Authority,
    Tls,
    Redirect,
    Proxy,
    Credential,
}

/// The principal allowed to supply one connection field.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionFieldOwner {
    Author,
    Environment,
    System,
}

/// Canonical ownership for one connection field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ConnectionFieldOwnership {
    pub field: ConnectionField,
    pub owner: ConnectionFieldOwner,
}

/// The authority interpretation fixed by a connection type.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionAuthorityModel {
    HttpOrigin,
}

/// How environment-owned credentials enter a request.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialInjection {
    EnvironmentSelectedHttpHeader,
}

/// Versioned portable semantics for one connection type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ConnectionTypeDescriptor {
    pub descriptor_version: String,
    pub requirement_type: String,
    pub contract: String,
    pub authority_model: ConnectionAuthorityModel,
    pub field_ownership: Vec<ConnectionFieldOwnership>,
    pub credential_injection: CredentialInjection,
}

impl ConnectionTypeDescriptor {
    /// The minimum portable HTTP connection descriptor.
    pub fn http_v1() -> Self {
        Self {
            descriptor_version: CONNECTION_DESCRIPTOR_VERSION.to_string(),
            requirement_type: "http".to_string(),
            contract: "wamn:connection/http@0.1.0".to_string(),
            authority_model: ConnectionAuthorityModel::HttpOrigin,
            field_ownership: vec![
                ConnectionFieldOwnership {
                    field: ConnectionField::Method,
                    owner: ConnectionFieldOwner::Author,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::RelativeTarget,
                    owner: ConnectionFieldOwner::Author,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::Headers,
                    owner: ConnectionFieldOwner::Author,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::Body,
                    owner: ConnectionFieldOwner::Author,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::Authority,
                    owner: ConnectionFieldOwner::Environment,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::Tls,
                    owner: ConnectionFieldOwner::Environment,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::Redirect,
                    owner: ConnectionFieldOwner::Environment,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::Proxy,
                    owner: ConnectionFieldOwner::Environment,
                },
                ConnectionFieldOwnership {
                    field: ConnectionField::Credential,
                    owner: ConnectionFieldOwner::Environment,
                },
            ],
            credential_injection: CredentialInjection::EnvironmentSelectedHttpHeader,
        }
    }

    /// Stable bytes embedded in persisted connection requirement identities.
    pub fn identity_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("connection descriptor identity serializes")
    }
}

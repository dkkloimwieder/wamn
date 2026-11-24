//! Connection rows and credential references from project databases.

use serde_json::Value;
use uuid::Uuid;

/// Mutable authority and lease coordinates of one environment connection.
#[derive(Debug)]
pub struct ConnectionFact {
    pub active_generation: i64,
    pub active: bool,
    pub credential_id: Option<Uuid>,
    pub target_id: Uuid,
    pub resource_key: Option<String>,
    pub authority: Option<String>,
    pub service_principal_id: wamn_principal::PrincipalId,
}

/// One generation of a connection, apart from its secret.
#[derive(Clone, Debug)]
pub struct ConnectionGeneration {
    pub name: String,
    pub integration_type: crate::integration_code::IntegrationCode,
    pub auth_type: String,
    pub external_system: Option<String>,
    pub parameters: Value,
}

/// A reference to credential material in the configured secret store.
#[derive(Debug)]
pub struct CredentialReference {
    pub reference: String,
    pub version: String,
}

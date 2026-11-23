//! Connection rows and credential references from project databases.

use serde_json::Value;
use std::collections::BTreeMap;
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
    pub service_principal_id: wamn_identity_protocol::PrincipalId,
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

/// Connection facts and credential references in a project.
pub trait ConnectionStore {
    type Error;

    /// Read current project connection status, target and principal.
    ///
    /// # Errors
    /// Returns database errors.
    fn fact(
        &mut self,
        id: &Uuid,
    ) -> impl std::future::Future<Output = Result<Option<ConnectionFact>, Self::Error>> + Send;

    /// Read immutable generation data; workload configuration supplies its runtime type.
    ///
    /// # Errors
    /// Returns database errors.
    fn generation(
        &mut self,
        id: &Uuid,
        generation: i64,
    ) -> impl std::future::Future<Output = Result<Option<ConnectionGeneration>, Self::Error>> + Send;

    /// Read an immutable secret reference in the selected project database.
    ///
    /// # Errors
    /// Returns database errors.
    fn credential(
        &mut self,
        id: &Uuid,
    ) -> impl std::future::Future<Output = Result<Option<CredentialReference>, Self::Error>> + Send;

    /// Read local connection names and identities.
    ///
    /// # Errors
    /// Returns database errors.
    fn environment(
        &mut self,
    ) -> impl std::future::Future<Output = Result<BTreeMap<String, Uuid>, Self::Error>> + Send;
}

//! Persistence operations used by connection administration.

use serde_json::Value;
use uuid::Uuid;
use wamn_identity_protocol::PrincipalId;
use wamn_registry::OrgId;

use crate::administration::ConnectionError;

/// Whether administration needs to hold the row until transaction end.
#[derive(Debug, Clone, Copy)]
pub enum Lookup {
    Read,
    Lock,
}

/// Data for a new connection before its first generation is written.
#[derive(Debug)]
pub struct NewConnection<'a> {
    pub id: Uuid,
    pub name: Option<&'a str>,
    pub integration: Option<&'a str>,
    pub auth: &'a str,
    pub external: Option<Uuid>,
    pub target: Uuid,
    pub resource: Option<String>,
    pub principal: PrincipalId,
    pub authority: Option<&'a str>,
}

/// How a credential or parameter change affects the current token.
#[derive(Debug, Clone, Copy)]
pub enum TokenChange {
    Keep,
    Delete,
    Expire,
}

/// Integration type, auth type, parameters and authority from the source.
pub type CopySource = (String, String, Value, Option<String>);

/// Named operations on the caller's open project administration transaction.
#[async_trait::async_trait]
pub trait AdministrationStore: Send {
    async fn connection_id(
        &mut self,
        name: Option<&str>,
        lookup: Lookup,
    ) -> Result<Option<Uuid>, ConnectionError>;
    async fn lock_connection(
        &mut self,
        name: Option<&str>,
    ) -> Result<Option<Value>, ConnectionError>;
    async fn integration_metadata(
        &mut self,
        integration: Option<&str>,
        auth: Option<&str>,
    ) -> Result<Option<Value>, ConnectionError>;
    async fn target(
        &mut self,
        caller: PrincipalId,
        operation: &str,
        org: &OrgId,
        env: Option<&str>,
        exclusive: bool,
    ) -> Result<Uuid, ConnectionError>;
    async fn versions(&mut self, integration: Option<&str>) -> Result<Vec<Value>, ConnectionError>;
    async fn previous(
        &mut self,
        id: Uuid,
        generation: Option<i64>,
    ) -> Result<Option<(Value, Option<Uuid>)>, ConnectionError>;
    async fn copy_source(
        &mut self,
        name: Option<&str>,
    ) -> Result<Option<CopySource>, ConnectionError>;
    async fn required_version(
        &mut self,
        caller: PrincipalId,
        operation: &str,
        org: &OrgId,
        env: &str,
        name: &str,
    ) -> Result<Option<i32>, ConnectionError>;
    async fn external_system_id(
        &mut self,
        name: Option<&str>,
        lookup: Lookup,
    ) -> Result<Option<Uuid>, ConnectionError>;
    async fn write_service(
        &mut self,
        name: &str,
        connection: &str,
    ) -> Result<PrincipalId, ConnectionError>;
    async fn resource_value(
        &mut self,
        parameters: &Value,
        field: &str,
    ) -> Result<Option<String>, ConnectionError>;
    async fn insert_connection(&mut self, row: NewConnection<'_>) -> Result<(), ConnectionError>;
    async fn write_generation(
        &mut self,
        id: Uuid,
        credential_id: Option<Uuid>,
        stored: Option<&Value>,
        token_change: TokenChange,
        parameters: &Value,
    ) -> Result<(), ConnectionError>;
    async fn configure_integration_type(
        &mut self,
        org: &OrgId,
        operation: &str,
        request: &Value,
    ) -> Result<Value, ConnectionError>;
    async fn list_external_systems(&mut self) -> Result<Value, ConnectionError>;
    async fn insert_external_system(&mut self, name: Option<&str>) -> Result<(), ConnectionError>;
    async fn delete_external_system(&mut self, id: Uuid) -> Result<(), ConnectionError>;
    async fn refuse_environment_references(
        &mut self,
        group: &str,
        id: &str,
    ) -> Result<(), ConnectionError>;
    async fn connection_ids(&mut self) -> Result<Vec<Uuid>, ConnectionError>;
    async fn test_metadata(
        &mut self,
        integration: Option<&str>,
        auth: Option<&str>,
    ) -> Result<Value, ConnectionError>;
    async fn credential_reference(&mut self, id: Uuid) -> Result<Value, ConnectionError>;
    async fn set_connection_status(
        &mut self,
        id: Uuid,
        status: &str,
    ) -> Result<(), ConnectionError>;
    async fn delete_connection(&mut self, id: Uuid) -> Result<Value, ConnectionError>;
    async fn connection_summary(&mut self, id: Uuid) -> Result<Value, ConnectionError>;
    async fn version_summary(
        &mut self,
        integration: Option<&str>,
    ) -> Result<Value, ConnectionError>;
}

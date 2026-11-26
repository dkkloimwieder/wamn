//! Native operation adapters. Route admission always precedes these calls.

use serde_json::Value;
use wamn_identity_protocol::PrincipalId;

mod service;

#[derive(Debug)]
pub(crate) struct Execution {
    pub org: wamn_registry::OrgId,
    pub worker: wamn_kubernetes::job::Worker,
    pub logins: std::sync::Arc<crate::project_login::ProjectLogins>,
    pub control: sqlx::PgPool,
    pub registry_token_metadata: bool,
}

impl Execution {
    /// Run a native control operation after route admission.
    pub(crate) async fn execute(
        &self,
        operation: &wamn_package::canonical_name::CanonicalName,
        caller: &PrincipalId,
        payload: &str,
        command: Option<&crate::platform_event::Command>,
    ) -> anyhow::Result<Value> {
        match operation.as_str() {
            "environment.configure" | "environment.delete" | "environment.operation_status" => {
                self.env(operation, payload, command).await
            }
            "api_service.create"
            | "service_principal.create"
            | "service_principal.disable"
            | "pat.issue"
            | "pat.list"
            | "pat.revoke" => self.service(operation, caller, payload).await,
            "platform.schema" | "platform.operation_status" => {
                self.platform_schema(operation, payload, command).await
            }
            _ => anyhow::bail!("unknown control execution operation {operation}"),
        }
    }
}

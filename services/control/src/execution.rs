//! Native operation adapters. Route admission always precedes these calls.

use std::path::PathBuf;

use serde_json::Value;
use wamn_identity_protocol::PrincipalId;

mod service;

#[derive(Debug)]
pub(crate) struct Execution {
    pub org: wamn_registry::OrgId,
    pub worker: wamn_kubernetes::job::Worker,
    pub administration: PathBuf,
    pub control: sqlx::PgPool,
    pub registry_token_metadata: bool,
}

#[async_trait::async_trait]
impl crate::host_route::ControlExecution for Execution {
    async fn execute(
        &self,
        operation: &wamn_package::canonical_name::CanonicalName,
        caller: &PrincipalId,
        payload: &str,
    ) -> anyhow::Result<Value> {
        match operation.as_str() {
            "environment.configure" | "environment.delete" | "environment.operation_status" => {
                self.environment(operation, payload).await
            }
            "api_service.create"
            | "service_principal.create"
            | "service_principal.disable"
            | "pat.issue"
            | "pat.list"
            | "pat.revoke" => self.service(operation, caller, payload).await,
            "platform.schema" | "platform.operation_status" => {
                self.platform_schema(operation, payload).await
            }
            _ => anyhow::bail!("unknown control execution operation {operation}"),
        }
    }
}

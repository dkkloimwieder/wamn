//! Native operation adapters. Route admission always precedes these calls.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};
use wamn_identity_protocol::PrincipalId;

mod service;

#[derive(Debug)]
pub(crate) struct Execution {
    pub org: wamn_registry::OrgId,
    pub worker: wamn_kubernetes::job::Worker,
    pub administration: PathBuf,
    pub control_database: String,
    pub registry_token_metadata: bool,
    pub system_database: String,
    pub registry_credentials: wamn_oci::package_artifact::registry_credentials::RegistryCredentialSource,
    pub integration_registry: wamn_oci::package_artifact::PackageRegistry,
}

#[async_trait::async_trait]
impl crate::host_route::ControlExecution for Execution {
    async fn execute(
        &self,
        operation: &wamn_catalog::canonical_name::CanonicalName,
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
            "platform.update" | "platform.operation_status" => {
                self.platform(operation, payload).await
            }
            "integration.publish" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Request {
                    code: wamn_registry::integration_code::IntegrationCode,
                    digest: wamn_digest::Digest,
                    version: i32,
                    parameter_schema: Value,
                    operations: String,
                    write_interfaces: Vec<String>,
                }
                let request: Request = serde_json::from_str(payload)?;
                let mut registry = self.integration_registry.clone();
                let base = wamn_catalog::parse_component_artifact_base(&registry.artifact_base)?;
                registry.credentials = Some(
                    self.registry_credentials
                        .credentials(base.registry())
                        .await?,
                );
                wamn_publish::integration::record(
                    &registry,
                    &self.system_database,
                    &request.code,
                    &request.digest,
                    &wamn_publish::integration::Publication {
                        version: request.version,
                        parameter_schema: request.parameter_schema,
                        operations: request.operations,
                        write_interfaces: request.write_interfaces,
                    },
                )
                .await?;
                Ok(json!({"code":request.code,"version":request.version,"digest":request.digest}))
            }
            _ => anyhow::bail!("unknown control execution operation {operation}"),
        }
    }
}

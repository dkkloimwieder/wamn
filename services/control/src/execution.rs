//! Native operation adapters. Route admission always precedes these calls.

use std::{path::PathBuf, sync::Arc};

use serde::Deserialize;
use serde_json::{Value, json};
use wamn_environment::environment::Platform;
use wamn_identity_protocol::PrincipalId;

mod service;

#[derive(Debug)]
pub(crate) struct Execution {
    pub org: wamn_registry::OrgId,
    pub worker: wamn_kubernetes::job::Worker,
    pub platform_database: wamn_postgres::ConnectionConfig,
    pub event_observer: Option<wamn_broker::nats::PlatformNats>,
    pub administration: PathBuf,
    pub control_database: String,
    pub platform: Arc<Platform>,
    pub system_database: String,
    pub registry_credentials: wamn_oci::package_artifact::registry_credentials::RegistryCredentialSource,
    pub integration_registry: wamn_oci::package_artifact::PackageRegistry,
}

impl Execution {
    pub(crate) fn application_database(
        &self,
        project: &wamn_registry::ProjectId,
        environment: &wamn_registry::EnvName,
    ) -> anyhow::Result<String> {
        wamn_provision::validate_project_env(&self.org, project, environment)?;
        let key = wamn_provision::control_administration_key(project, environment);
        let config = std::fs::read_to_string(self.administration.join(key))?;
        wamn_provision::migrations::MigrationDatabase::Project.configuration(&config)
    }
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
                self.environment(operation, caller, payload).await
            }
            "api_service.create" | "service_principal.create" | "service_principal.disable"
            | "pat.issue" | "pat.list" | "pat.revoke" => {
                self.service(operation, caller, payload).await
            }
            "platform.update" | "platform.operation_status" => {
                self.platform(operation, caller, payload).await
            }
            "capture_gap.recover" | "capture_gap.close" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Request {
                    project: wamn_registry::ProjectId,
                    env: wamn_registry::EnvName,
                }
                let request: Request = serde_json::from_str(payload)?;
                if operation == "capture_gap.close" {
                    let closed = wamn_environment::environment::capture_gap::close(
                        &self.platform.target,
                        &wamn_environment::environment::capture_gap::CloseCaptureGapRequest {
                            org: self.org.clone(),
                            project: request.project,
                            env: request.env,
                            system_database_url: self.system_database.clone(),
                        },
                    )
                    .await?;
                    return Ok(
                        json!({"slot":closed.slot,"created_at":closed.created_at.to_rfc3339(),"resync_at":closed.resync_at.to_rfc3339()}),
                    );
                }
                let triple = wamn_registry::Triple::new(
                    self.org.clone(),
                    request.env.clone(),
                    request.project.clone(),
                );
                let instance = wamn_environment::provision_project_env::read_project_env_instance(
                    &self.platform.system_database_url,
                    &triple,
                )
                .await?;
                let mut database: wamn_postgres::ConnectionConfig = serde_json::from_str(
                    &self.application_database(&request.project, &request.env)?,
                )?;
                database.username = wamn_provision::cdc_object_name(
                    &self.org,
                    &request.project,
                    &request.env,
                    &instance,
                )
                .parse()?;
                database.credentials.name = wamn_provision::project_env_cdc_secret_name(
                    &self.org,
                    &request.project,
                    &request.env,
                );
                let observer = self
                    .event_observer
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("event observer configuration is absent"))?;
                let gap = wamn_environment::environment::capture_gap::recover(
                    &self.platform.target,
                    &wamn_environment::environment::capture_gap::RecoverCaptureGapRequest {
                        org: self.org.clone(),
                        project: request.project,
                        env: request.env,
                        system_database_url: self.system_database.clone(),
                        admin_database_url: database.to_json()?,
                        nats: observer.clone(),
                    },
                )
                .await?;
                Ok(
                    json!({"slot":gap.slot,"reason":gap.reason,"start_lsn":gap.start_lsn,"start_at":gap.start_at.to_rfc3339(),"end_lsn":gap.end_lsn,"created_at":gap.created_at.to_rfc3339()}),
                )
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

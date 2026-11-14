//! Native operation adapters. Route admission always precedes these calls.

use std::{path::PathBuf, sync::Arc, time::Duration};

use serde::Deserialize;
use serde_json::{Value, json};
use wamn_environment::environment::Platform;
use wamn_identity_protocol::PrincipalId;

#[derive(Debug)]
pub(crate) struct Execution {
    pub org: String,
    pub worker: wamn_kubernetes::job::Worker,
    pub platform_database: wamn_postgres::ConnectionConfig,
    pub event_observer: Option<wamn_broker::nats::PlatformNats>,
    pub changes: wamn_broker::Publisher,
    pub administration: PathBuf,
    pub identity: wamn_identity_client::PatIssuerConfig,
    pub platform: Arc<Platform>,
    pub system_database: String,
    pub registry_credentials: wamn_package_artifact::registry_credentials::RegistryCredentialSource,
    pub integration_registry: wamn_package_artifact::PackageRegistry,
}

impl Execution {
    pub(crate) fn application_database(
        &self,
        project: &str,
        environment: &str,
    ) -> anyhow::Result<String> {
        wamn_control_provision::validate_project_env(&self.org, project, environment)?;
        let key = wamn_control_provision::control_administration_key(project, environment);
        let config = std::fs::read_to_string(self.administration.join(key))?;
        wamn_control_provision::migrations::MigrationDatabase::Project.configuration(&config)
    }
}

#[async_trait::async_trait]
impl wamn_execution_host::ControlExecution for Execution {
    async fn execute(
        &self,
        operation: &str,
        caller: &PrincipalId,
        payload: &str,
    ) -> anyhow::Result<Value> {
        match operation {
            "environment.configure" | "environment.delete" | "environment.operation_status" => {
                self.environment(operation, caller, payload).await
            }
            "pat.revoke" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Request {
                    token_prefix: String,
                }
                let request: Request = serde_json::from_str(payload)?;
                let mut connection = wamn_postgres::connect(&self.system_database).await?;
                let allowed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT FROM identity.pats t JOIN identity.principals p ON p.id=t.principal_id WHERE t.token_prefix=$1 AND (p.id=$2::text::uuid OR EXISTS(SELECT FROM identity.project_roles r WHERE r.principal_id=p.id AND r.org=$3) OR (p.type='service' AND starts_with(p.subject,'api.'||$3||'.'))))")
                    .bind(&request.token_prefix).bind(caller.to_string()).bind(&self.org).fetch_one(&mut connection).await?;
                anyhow::ensure!(
                    allowed,
                    "PAT does not belong to the authenticated org authority"
                );
                wamn_environment::provision_project_env::revoke_provisioning_pat(
                    &self.system_database,
                    &request.token_prefix,
                    &self.changes,
                )
                .await?;
                Ok(json!({"revoked":true}))
            }
            "platform.update" | "platform.operation_status" => {
                self.platform(operation, caller, payload).await
            }
            "capture_gap.recover" | "capture_gap.close" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Request {
                    project: String,
                    env: String,
                }
                let request: Request = serde_json::from_str(payload)?;
                if operation == "capture_gap.close" {
                    let closed = wamn_environment::environment::capture_gap::close(
                        &self.platform,
                        &wamn_environment::capture_gap::CloseCaptureGapRequest {
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
                let triple =
                    wamn_registry::Triple::new(&self.org, request.env.as_str(), &request.project);
                let instance = wamn_environment::provision_project_env::read_project_env_instance(
                    &self.platform.system_database_url,
                    &triple,
                )
                .await?;
                let mut database: wamn_postgres::ConnectionConfig = serde_json::from_str(
                    &self.application_database(&request.project, &request.env)?,
                )?;
                database.username = wamn_control_provision::cdc_object_name(
                    &self.org,
                    &request.project,
                    &request.env,
                    &instance,
                );
                database.credentials.name = wamn_control_provision::project_env_cdc_secret_name(
                    &self.org,
                    &request.project,
                    &request.env,
                );
                let observer = self
                    .event_observer
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("event observer configuration is absent"))?;
                let gap = wamn_environment::environment::capture_gap::recover(
                    &self.platform,
                    &wamn_environment::capture_gap::RecoverCaptureGapRequest {
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
            "api_service.create" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Request {
                    project: String,
                    env: String,
                    name: String,
                    display_name: String,
                    roles: Vec<wamn_identity_protocol::RoleName>,
                    lifetime_seconds: u64,
                }
                let request: Request = serde_json::from_str(payload)?;
                let token = wamn_environment::api_service::create(
                    &wamn_environment::api_service::Request {
                        application_database_url: self
                            .application_database(&request.project, &request.env)?,
                        org: self.org.clone(),
                        project: request.project,
                        env: request.env,
                        name: request.name,
                        display_name: request.display_name,
                        roles: request.roles,
                        lifetime: Duration::from_secs(request.lifetime_seconds),
                        system_database_url: self.system_database.clone(),
                        identity: self.identity.clone(),
                    },
                    &self.changes,
                )
                .await?;
                Ok(serde_json::to_value(token)?)
            }
            "integration.publish" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Request {
                    code: String,
                    digest: String,
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
                wamn_package_control::integration::record(
                    &registry,
                    &self.system_database,
                    &request.code,
                    &request.digest,
                    &wamn_package_control::integration::Publication {
                        version: request.version,
                        parameter_schema: request.parameter_schema,
                        operations: request.operations,
                        write_interfaces: request.write_interfaces,
                    },
                )
                .await?;
                Ok(json!({"code":request.code,"version":request.version,"digest":request.digest}))
            }
            _ => self
                .workflow(operation, caller, payload)
                .await
                .map(|result| json!({"result":result})),
        }
    }
}

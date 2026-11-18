//! Service administration selects one database from the org's administration map.

use anyhow::{Context as _, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Connection as _;
use std::time::Duration;
use wamn_catalog::canonical_name::CanonicalName;
use wamn_environment::api_service;
use wamn_identity::application::service;
use wamn_identity_protocol::{PrincipalId, RoleName, pat_prefix::PatPrefix};
use wamn_registry::{EnvName, ProjectId};

use super::Execution;

#[derive(Deserialize)]
struct Target {
    project: ProjectId,
    env: EnvName,
}

impl Execution {
    pub(super) async fn service(
        &self,
        operation: &CanonicalName,
        caller: &PrincipalId,
        payload: &str,
    ) -> anyhow::Result<Value> {
        let target: Target = serde_json::from_str(payload)?;
        let mut input: serde_json::Map<String, Value> = serde_json::from_str(payload)?;
        input.remove("project");
        input.remove("env");
        let input = Value::Object(input);
        wamn_provision::validate_project_env(&self.org, &target.project, &target.env)?;
        let key = wamn_provision::control_administration_key(&target.project, &target.env);
        let login = wamn_postgres::ConnectionConfig::read(&self.administration.join(key))
            .await
            .context("read the project's administration login")?;
        let mut system_connection = wamn_postgres::connect(&self.control_database)
            .await
            .context("connect the control login for service administration")?;
        let mut system =
            wamn_identity::transaction::read_connection(&mut system_connection, caller).await?;
        let instance: wamn_registry::instance_suffix::InstanceSuffix = sqlx::query_scalar(
            "SELECT instance_suffix FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3",
        )
        .bind(&self.org)
        .bind(&target.project)
        .bind(&target.env)
        .fetch_optional(&mut *system)
        .await?
        .context("the project deployment does not exist")?;
        let domain = if matches!(
            operation.as_str(),
            "api_service.create" | "service_principal.create"
        ) {
            Some(service::platform_domain(&mut system).await?)
        } else {
            None
        };
        system.commit().await?;
        let expected = wamn_registry::name::project_env_database_name(
            &self.org,
            &target.project,
            &target.env,
            &instance,
        );
        ensure!(
            login.database == expected,
            "administration login names another project database"
        );
        let mut project =
            wamn_postgres::connect(login.mounted_options(&self.administration).await?)
                .await
                .context("connect the project's administration login")?;
        let mut transaction = project.begin().await?;
        if operation != "pat.list" {
            let sealed = wamn_catalog::operation_token(
                wamn_catalog::host_route_package(),
                wamn_catalog::host_route_version(),
                &operation.as_str().parse()?,
            )?;
            sqlx::query(
                "INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1,'','',$2)",
            )
            .bind(caller)
            .bind(&sealed)
            .execute(&mut *transaction)
            .await?;
        }
        let result = match operation.as_str() {
            "api_service.create" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Request {
                    name: String,
                    display_name: String,
                    roles: Vec<RoleName>,
                    lifetime_seconds: u64,
                }
                let request: Request = serde_json::from_value(input)?;
                let domain = domain.as_ref().expect("service creation read the domain");
                let key = wamn_registry::project_env_key(&self.org, &target.project, &target.env);
                let token = api_service::create(
                    &mut transaction,
                    &key,
                    domain,
                    &api_service::Request {
                        org: &self.org,
                        project: &target.project,
                        name: &request.name,
                        display_name: &request.display_name,
                        roles: &request.roles,
                        lifetime: Duration::from_secs(request.lifetime_seconds),
                    },
                )
                .await?;
                serde_json::to_value(token)?
            }
            "service_principal.create" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Request {
                    subject: String,
                    display_name: String,
                }
                let request: Request = serde_json::from_value(input)?;
                let domain = domain.as_ref().expect("service creation read the domain");
                let principal = service::write_service(
                    &mut transaction,
                    domain,
                    &request.subject,
                    &request.display_name,
                )
                .await?;
                json!({"principal_id":principal})
            }
            "service_principal.disable" | "pat.list" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Request {
                    principal_id: PrincipalId,
                }
                let request: Request = serde_json::from_value(input)?;
                if operation == "pat.list" {
                    json!({"pats":service::list(&mut transaction, &request.principal_id).await?})
                } else {
                    service::disable(&mut transaction, &request.principal_id).await?;
                    json!({"principal_id":request.principal_id,"status":"disabled"})
                }
            }
            "pat.issue" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Request {
                    principal_id: PrincipalId,
                    label: String,
                    lifetime_seconds: u64,
                }
                let request: Request = serde_json::from_value(input)?;
                let token = service::issue(
                    &mut transaction,
                    &request.principal_id,
                    &request.label,
                    Duration::from_secs(request.lifetime_seconds),
                )
                .await?;
                serde_json::to_value(token)?
            }
            "pat.revoke" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Request {
                    token_prefix: PatPrefix,
                }
                let request: Request = serde_json::from_value(input)?;
                service::revoke(&mut transaction, &request.token_prefix).await?;
                json!({"revoked":true})
            }
            _ => unreachable!("the execution adapter selects service routes"),
        };
        transaction.commit().await?;
        Ok(result)
    }
}

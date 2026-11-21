//! Service administration selects one project database through its platform login.

use wamn_package::canonical_name::CanonicalName;
use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Connection as _;
use std::time::Duration;
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
        let mut system = wamn_identity::transaction::read_pool(&self.control, caller).await?;
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
        let triple = wamn_registry::Triple::new(self.org.clone(), target.env, target.project);
        let login = self
            .logins
            .login(&triple, &instance)
            .await
            .context("read the project's administration login")?;
        let mut project = wamn_postgres::connect(login)
            .await
            .context("connect the project's administration login")?;
        let mut transaction = project.begin().await?;
        if operation != "pat.list" {
            let sealed = wamn_package::operation_token(
                wamn_routes::host_route_package(),
                wamn_routes::host_route_version(),
                &operation.as_str().parse()?,
            )?;
            wamn_postgres::bind_transaction_identity(
                &mut transaction,
                caller,
                "",
                "",
                Some(sealed.as_str()),
                None,
            )
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
                let key = wamn_registry::project_env_key(&self.org, &triple.project, &triple.env);
                let token = api_service::create(
                    &mut transaction,
                    &key,
                    domain,
                    &api_service::Request {
                        org: &self.org,
                        project: &triple.project,
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

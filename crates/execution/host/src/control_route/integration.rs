//! Integration administration under project authority and environment locks.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};
use std::time::Duration;

use anyhow::Context as _;
use serde_json::{Value, json};

use wamn_broker::auth_changes::AuthDrops;
use wamn_broker::connection_changes::ConnectionDrops;
use wamn_catalog::HostAttachment;
use wamn_identity_protocol::PrincipalId;

use super::{ControlRoutes, ProjectTarget, Refusal, incomplete, parse};
use wamn_platform_identity::connections::{ConnectionError, administration, references};

impl From<ConnectionError> for Refusal {
    fn from(error: ConnectionError) -> Self {
        if matches!(error.code, "database_error" | "store_error") {
            Self::Failed(error.into())
        } else {
            Self::Declared {
                code: error.code,
                detail: json!({"field":error.field}),
            }
        }
    }
}

impl ControlRoutes<'_> {
    pub(super) async fn integration_administration(
        &self,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        if attachment.route.operation == "connection.test" {
            return self.connection_test(attachment, caller, payload).await;
        }
        let mut writer = self.writer.lock().await;
        let result = self
            .integration_administration_in(&mut writer, attachment, caller, payload)
            .await;
        sqlx::query("SELECT pg_advisory_unlock_all()")
            .fetch_one(&mut *writer)
            .await?;
        result
    }

    async fn integration_administration_in(
        &self,
        writer: &mut Client,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let project = parse::<ProjectTarget>(payload)?.project;
        let operation = attachment.route.operation;
        let request: Value = serde_json::from_str(payload).map_err(|_| Refusal::Declared {
            code: "invalid_input",
            detail: json!({"field":"value"}),
        })?;
        let mut transaction = self
            .begin_in_environment(
                writer,
                attachment,
                caller,
                Some(&project),
                request["env"].as_str(),
            )
            .await?;
        if operation == "connection.copy"
            && !self
                .admits_environment(
                    &mut transaction,
                    caller,
                    Some(&project),
                    request["source_env"].as_str(),
                )
                .await?
        {
            return Err(super::denied(attachment));
        }
        let mut drops = AuthDrops::default();
        let mut connection_drops = ConnectionDrops::default();
        if operation == "connection.authorize" {
            let oauth = self.oauth.ok_or_else(|| {
                Refusal::Failed(anyhow::anyhow!("OAuth authorization unavailable"))
            })?;
            let context = administration::execute(
                &mut transaction,
                self.org,
                operation,
                payload,
                &mut drops,
                &mut connection_drops,
            )
            .await?;
            let caller = caller
                .to_string()
                .as_str()
                .parse::<uuid::Uuid>()
                .map_err(|error| Refusal::Failed(error.into()))?;
            let url = oauth
                .authorize(&mut transaction, &context, caller)
                .await
                .map_err(|error| Refusal::Failed(error.into()))?;
            transaction.commit().await?;
            return Ok(json!({"authorization_url":url}));
        }
        if attachment.route.type_.is_read() {
            let mut result = administration::execute(
                &mut transaction,
                self.org,
                operation,
                payload,
                &mut drops,
                &mut connection_drops,
            )
            .await?;
            if operation == "connection.get" && result["auth_type"] == "oauth2_authorization_code" {
                result["redirect_uri"] = self
                    .oauth
                    .ok_or_else(|| {
                        Refusal::Failed(anyhow::anyhow!("OAuth authorization unavailable"))
                    })?
                    .redirect_uri()
                    .into();
            }
            transaction.commit().await?;
            return Ok(result);
        }
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('connections:' || $1,0))")
            .bind(self.org)
            .fetch_one(&mut *transaction)
            .await?;
        let deleting = operation.ends_with(".delete");
        let group = operation.split_once('.').expect("route group").0;
        if deleting {
            administration::configure(
                &mut transaction,
                self.org,
                "integration.lock_project",
                &json!({"project":project}),
            )
            .await?;
        }
        let projection_env = if group == "connection" && !deleting {
            request["env"].as_str()
        } else {
            None
        };
        let environments: Vec<(String, String)> = sqlx::query(wamn_registry::sql::EXECUTION_HOST_SRC_CONTROL_ROUTE_INTEGRATION_INTEGRATION_ADMINISTRATION_IN).bind(self.org).bind(&project).bind(projection_env).fetch_all(&mut *transaction)
            .await?
            .into_iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect();
        if group == "connection" {
            for (_, env) in &environments {
                let held: bool=sqlx::query("SELECT pg_try_advisory_xact_lock(hashtextextended('wamn-env/' || $1 || '/' || $2 || '/' || $3,0))").bind(self.org).bind(&project).bind(env).fetch_one(&mut *transaction).await?.get(0);
                if !held {
                    return Err(Refusal::Declared {
                        code: "deployment_in_progress",
                        detail: json!({"field":"env"}),
                    });
                }
            }
        }
        let identity = if deleting {
            let id = deletion_identity(&mut transaction, self.org, group, &request).await?;
            for (_, env) in &environments {
                let held: bool = sqlx::query("SELECT pg_try_advisory_lock(hashtextextended('wamn-env/' || $1 || '/' || $2 || '/' || $3,0))").bind(self.org).bind(&project).bind(env).fetch_one(&mut *transaction).await?.get(0);
                if !held {
                    return Err(Refusal::Declared {
                        code: "deployment_in_progress",
                        detail: json!({"field":"env"}),
                    });
                }
            }
            Some(id)
        } else {
            None
        };
        // Keep every deletion preflight transaction open until the authority commits.
        let logins = self.logins(&environments)?;
        let mut clients = Vec::new();
        for url in logins {
            let client = tokio::time::timeout(Duration::from_secs(5), wamn_postgres::connect(&url))
                .await
                .context("environment connection timed out")??;
            clients.push(client);
        }
        let mut targets = Vec::new();
        for client in &mut clients {
            let mut target = client.begin().await?;
            sqlx::query(
                "SELECT set_config('app.user_id',$1,true),set_config('app.operation',$2,true)",
            )
            .bind(caller.to_string().as_str())
            .bind(&attachment.operation)
            .execute(&mut *target)
            .await
            .map(|result| result.rows_affected())?;
            if let Some(id) = &identity {
                references::refuse_environment_references(&mut target, group, id).await?;
            }
            targets.push(target);
        }
        let mut result = if group == "connection"
            && let Some(target) = targets.first_mut()
        {
            administration::execute_in_environment(
                &mut transaction,
                target,
                self.org,
                operation,
                payload,
                &mut drops,
                &mut connection_drops,
            )
            .await?
        } else {
            administration::execute(
                &mut transaction,
                self.org,
                operation,
                payload,
                &mut drops,
                &mut connection_drops,
            )
            .await?
        };
        if let Some(principal) = result["principal_id"].as_str() {
            drops.identity(principal);
        }
        let credentials = result
            .as_object_mut()
            .and_then(|result| result.remove("credentials"));
        transaction.commit().await?;
        self.changes.publish(drops).await;
        self.changes.publish(connection_drops).await;
        let mut source = self
            .begin_in_environment(
                writer,
                attachment,
                caller,
                Some(&project),
                request["env"].as_str(),
            )
            .await?;
        let mut completed = Vec::new();
        let mut projected_drops = Vec::new();
        for (mut target, (project, env)) in targets.into_iter().zip(&environments) {
            let projected = async {
                if let Some(id) = &identity {
                    references::remove_projection(&mut target, group, id).await?;
                } else {
                    projected_drops.push(
                        references::project_administration(
                            &mut source,
                            &mut target,
                            self.org,
                            project,
                            env,
                        )
                        .await?,
                    );
                }
                target.commit().await?;
                Ok::<_, ConnectionError>(())
            }
            .await;
            if let Err(error) = projected {
                tracing::warn!(%error,project,env,"integration projection failed");
                return Err(incomplete(project, env, &completed));
            }
            completed.push(format!("{project}/{env}"));
        }
        source.commit().await?;
        for drops in projected_drops {
            self.changes.publish(drops).await;
        }
        if let Some(credentials) = credentials {
            references::destroy_credentials(&credentials).await?;
        }
        Ok(result)
    }
}

async fn deletion_identity(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: &str,
    group: &str,
    request: &Value,
) -> Result<String, Refusal> {
    let result = match group {
        "connection" => administration::configure(tx, org, "connection.get", request).await?,
        "integration" => administration::configure(tx, org, "integration.get", request).await?,
        "external_system" => {
            let rows = administration::configure(tx, org, "external_system.list", request).await?;
            rows["external_systems"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|row| row["name"] == request["name"])
                .cloned()
                .unwrap_or(Value::Null)
        }
        _ => unreachable!("administration route group"),
    };
    result["id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Refusal::Declared {
            code: "not_found",
            detail: json!({"field":"name"}),
        })
}

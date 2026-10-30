//! Integration administration under project authority and environment locks.

use std::time::Duration;

use anyhow::Context as _;
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_catalog::HostAttachment;
use wamn_platform_identity::{PrincipalId, auth_changes::AuthDrops};

use super::{ControlRoutes, ProjectScope, Refusal, incomplete, parse};
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
        let mut writer = self.writer.lock().await;
        let result = self
            .integration_administration_in(&mut writer, attachment, caller, payload)
            .await;
        writer
            .query_one("SELECT pg_advisory_unlock_all()", &[])
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
        let project = parse::<ProjectScope>(payload)?.project;
        let operation = attachment.route.operation;
        let request: Value = serde_json::from_str(payload).map_err(|_| Refusal::Declared {
            code: "invalid_input",
            detail: json!({"field":"value"}),
        })?;
        let transaction = self
            .begin_in(writer, attachment, caller, Some(&project))
            .await?;
        let mut drops = AuthDrops::default();
        if attachment.route.type_.is_read() {
            let result =
                administration::execute(&transaction, self.org, operation, payload, &mut drops)
                    .await?;
            transaction.commit().await?;
            return Ok(result);
        }
        transaction
            .query_one(
                "SELECT pg_advisory_xact_lock(hashtextextended('connections:' || $1,0))",
                &[&self.org],
            )
            .await?;
        let deleting = operation.ends_with(".delete");
        let group = operation.split_once('.').expect("route group").0;
        if deleting {
            administration::configure(
                &transaction,
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
        let environments: Vec<(String, String)> = transaction
            .query(
                "SELECT project,env FROM registry.project_envs WHERE org=$1 AND project=$2 \
             AND ($3::text IS NULL OR env=$3) ORDER BY env",
                &[&self.org, &project, &projection_env],
            )
            .await?
            .into_iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect();
        if group == "connection" {
            for (_, env) in &environments {
                let held: bool=transaction.query_one(
                    "SELECT pg_try_advisory_xact_lock(hashtextextended('wamn-env/' || $1 || '/' || $2 || '/' || $3,0))", &[&self.org,&project,env],
                ).await?.get(0);
                if !held {
                    return Err(Refusal::Declared {
                        code: "deployment_in_progress",
                        detail: json!({"field":"env"}),
                    });
                }
            }
        }
        let identity = if deleting {
            let id = deletion_identity(&transaction, self.org, group, &request).await?;
            for (_, env) in &environments {
                let held: bool = transaction.query_one(
                    "SELECT pg_try_advisory_lock(hashtextextended('wamn-env/' || $1 || '/' || $2 || '/' || $3,0))",
                    &[&self.org, &project, &env],
                ).await?.get(0);
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
            let (client, driver) =
                tokio::time::timeout(Duration::from_secs(5), tokio_postgres::connect(&url, NoTls))
                    .await
                    .context("environment connection timed out")??;
            tokio::spawn(driver);
            clients.push(client);
        }
        let mut targets = Vec::new();
        for client in &mut clients {
            let target = client.transaction().await?;
            target
                .execute(
                    "SELECT set_config('app.user_id',$1,true),set_config('app.operation',$2,true)",
                    &[&caller.as_str(), &attachment.operation],
                )
                .await?;
            if let Some(id) = &identity {
                references::refuse_environment_references(&target, group, id).await?;
            }
            targets.push(target);
        }
        let mut result = if group == "connection"
            && let Some(target) = targets.first()
        {
            administration::execute_in_environment(
                &transaction,
                target,
                self.org,
                operation,
                payload,
                &mut drops,
            )
            .await?
        } else {
            administration::execute(&transaction, self.org, operation, payload, &mut drops).await?
        };
        if let Some(principal) = result["principal_id"].as_str() {
            drops.identity(principal);
        }
        let credentials = result
            .as_object_mut()
            .and_then(|result| result.remove("credentials"));
        transaction.commit().await?;
        self.changes.publish(drops).await;
        let source = self
            .begin_in(writer, attachment, caller, Some(&project))
            .await?;
        let mut completed = Vec::new();
        let mut projected_drops = Vec::new();
        for (target, (project, env)) in targets.into_iter().zip(&environments) {
            let projected = async {
                if let Some(id) = &identity {
                    references::remove_projection(&target, group, id).await?;
                } else {
                    projected_drops.push(
                        references::project_administration(
                            &source, &target, self.org, project, env,
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
    tx: &Transaction<'_>,
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

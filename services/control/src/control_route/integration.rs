//! Connection administration against one selected project database.

use super::{ControlRoutes, ProjectTarget, Refusal, parse};
use serde_json::{Value, json};
use sqlx::{Connection as _, Row as _};
use wamn_catalog::HostAttachment;
use wamn_connection::connections::{ConnectionError, administration};
use wamn_identity_protocol::PrincipalId;

impl From<ConnectionError> for Refusal {
    fn from(error: ConnectionError) -> Self {
        if matches!(error.code, "database_error" | "store_error") {
            Self::Failed(error.into())
        } else {
            let mut detail = json!({"field":error.field});
            if let Some(reason) = error.reason() {
                detail["reason"] = json!(reason);
            }
            Self::Declared {
                code: error.code,
                detail,
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
        writer: &mut sqlx::PgConnection,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let target: ProjectTarget = parse(payload)?;
        let operation = attachment.route.operation;
        let request: Value = serde_json::from_str(payload).map_err(|_| Refusal::Declared {
            code: "invalid_input",
            detail: json!({"field":"value"}),
        })?;
        administration::validate_request(operation, &request)?;
        let mut authorized = self
            .begin_in(
                writer,
                attachment,
                caller,
                Some(&target.project),
            )
            .await?;
        if operation == "integration_type.list" {
            let result = sqlx::query_scalar("SELECT registry.configure_integration_type($1,$2,$3)")
                .bind(self.org)
                .bind(operation)
                .bind(&request)
                .fetch_one(&mut *authorized)
                .await?;
            authorized.commit().await?;
            return Ok(result);
        }
        let env = target.env.as_ref().ok_or_else(|| Refusal::Declared {
            code: "invalid_input",
            detail: json!({"field":"env"}),
        })?;
        if !attachment.route.type_.is_read() {
            let held: bool = sqlx::query(wamn_registry::sql::ENVIRONMENT_LOCK_SQL)
                .bind(self.org)
                .bind(env)
                .fetch_one(&mut *authorized)
                .await?
                .get(0);
            if !held {
                return Err(Refusal::Declared {
                    code: "deployment_in_progress",
                    detail: json!({"field":"env"}),
                });
            }
        }
        authorized.commit().await?;
        let urls = self.logins(&[(target.project.clone(), env.clone())])?;
        let mut client = wamn_postgres::connect(&urls[0]).await?;
        let mut project = client.begin().await?;
        sqlx::query("INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1,'','',$2)").bind(caller).bind(&attachment.operation).execute(&mut *project).await?;
        let mut source_client;
        let mut source_tx;
        let source = if operation == "connection.copy" {
            let source_env = target.source_env.as_ref().expect("validated source env");
            let urls = self.logins(&[(target.project.clone(), source_env.clone())])?;
            source_client = wamn_postgres::connect(&urls[0]).await?;
            source_tx =
                wamn_identity::transaction::read_connection(&mut source_client, caller).await?;
            Some(&mut *source_tx)
        } else {
            None
        };
        let applications = if matches!(operation, "connection.delete" | "integration.delete") {
            crate::environment_status::applications(self.org, &target.project, env).await?
        } else {
            Vec::new()
        };
        let mut result = administration::execute(
            &mut project,
            writer,
            *caller,
            self.org,
            operation,
            payload,
            source,
            &applications,
        )
        .await?;
        if operation == "connection.authorize" {
            let oauth = self.oauth.ok_or_else(|| {
                Refusal::Failed(anyhow::anyhow!("OAuth authorization unavailable"))
            })?;
            let url = oauth
                .authorize(&mut project, &result, *caller)
                .await
                .map_err(|e| Refusal::Failed(e.into()))?;
            project.commit().await?;
            return Ok(json!({"authorization_url":url}));
        }
        project.commit().await?;
        if operation.ends_with(".delete") {
            administration::finish_delete(writer, *caller, self.org, &result).await?;
        }
        if let Some(row) = result.as_object_mut() {
            let _ = row.remove("credentials");
        }
        if operation == "connection.get" && result["auth_type"] == "oauth2_authorization_code" {
            result["redirect_uri"] = self
                .oauth
                .ok_or_else(|| Refusal::Failed(anyhow::anyhow!("OAuth authorization unavailable")))?
                .redirect_uri()
                .into();
        }
        Ok(result)
    }
}

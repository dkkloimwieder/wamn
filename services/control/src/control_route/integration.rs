//! Connection administration against one selected project database.

use super::{ControlRoutes, ProjectTarget, Refusal, parse};
use serde_json::{Value, json};
use sqlx::Connection as _;
use wamn_connection::administration::{self, ConnectionDetails, ConnectionError, Outcome};
use wamn_connection::connection_credentials::{KubernetesStore, destroy_credentials};
use wamn_connection::project_connections::{Administration, ConnectionRow, copy_source};
use wamn_identity_protocol::PrincipalId;
use wamn_registry::{EnvName, ProjectId};
use wamn_routes::HostAttachment;
use wamn_routes::host_wire::{
    ConnectionAuthorizeResult, ConnectionGetResult, ConnectionListResult, ConnectionSummary,
};

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
        let mut writer = self.writer.lock().await?;
        let result = self
            .integration_administration_in(&mut writer, attachment, caller, payload)
            .await;
        wamn_postgres::advisory_lock::unlock_all_advisory_locks(&mut writer).await?;
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
            .begin_in(writer, attachment, caller, Some(&target.project))
            .await?;
        if operation == "integration_type.list" {
            let result = wamn_connection::project_connections::configure_integration_type(
                &mut authorized,
                self.org,
                operation,
                &request,
            )
            .await?;
            authorized.commit().await?;
            return Ok(result);
        }
        let env = target.env.as_ref().ok_or_else(|| Refusal::Declared {
            code: "invalid_input",
            detail: json!({"field":"env"}),
        })?;
        if !attachment.route.type_.is_read() {
            let held =
                wamn_postgres::advisory_lock::try_env_lock(&mut authorized, self.org, env).await?;
            if !held {
                return Err(Refusal::Declared {
                    code: "deployment_in_progress",
                    detail: json!({"field":"env"}),
                });
            }
        }
        authorized.commit().await?;
        let source = if operation == "connection.copy" {
            let source_env = target.source_env.as_ref().expect("validated source env");
            let login = self.login(&target.project, source_env).await?;
            let mut client = wamn_postgres::connect(login).await?;
            let mut source =
                wamn_identity::identity::transaction::read_connection(&mut client, caller).await?;
            let row = copy_source(
                &mut source,
                request["name"].as_str().expect("validated name"),
            )
            .await?;
            source.rollback().await?;
            row
        } else {
            None
        };
        let namespace = self.logins.namespace(env).await?;
        let credentials_store = KubernetesStore::in_namespace(&namespace);
        let login = self.login(&target.project, env).await?;
        let mut client = wamn_postgres::connect(login).await?;
        let mut project = client.begin().await?;
        bind_actor(&mut project, attachment, caller).await?;
        let mut database = Administration::new(&mut project, writer, None);
        let outcome = if matches!(
            operation,
            "connection.write"
                | "connection.copy"
                | "connection.set_credential"
                | "connection.activate"
        ) {
            let write = administration::prepare_write(
                &mut database,
                wamn_http::wasm_authority::wasm_authority_port,
                administration::Request {
                    caller: *caller,
                    org: self.org,
                    operation,
                    payload: request,
                },
                source,
            )
            .await?;
            project.rollback().await?;

            // Publish only after the immutable Secret version exists.
            let staged = write
                .stage(&credentials_store, self.org, &target.project, env)
                .await?;
            let mut project = client.begin().await?;
            let result = async {
                bind_actor(&mut project, attachment, caller).await?;
                let mut database = Administration::new(
                    &mut project,
                    writer,
                    Some(|system, project, name, connection| {
                        Box::pin(
                            wamn_identity::identity::app::service::write_connection_service(
                                system, project, name, connection,
                            ),
                        )
                    }),
                );
                Ok::<_, Refusal>(staged.publish(&mut database, env).await?)
            }
            .await;
            match result {
                Ok(outcome) => {
                    // A lost commit acknowledgement leaves the version available to SQL.
                    project.commit().await?;
                    outcome
                }
                Err(error) => {
                    project.rollback().await?;
                    if let Err(error) = staged.discard(&credentials_store).await {
                        tracing::warn!(%error, "could not discard staged connection credential");
                    }
                    return Err(error);
                }
            }
        } else {
            let outcome =
                administration::configure(&mut database, self.org, operation, &request).await?;
            if matches!(outcome, Outcome::Authorize(_)) {
                project.rollback().await?;
            } else {
                project.commit().await?;
            }
            outcome
        };

        let value = match outcome {
            Outcome::Connection(details) => {
                let connection = summary(details, self.org, &target.project, env);
                if operation == "connection.get" {
                    let redirect_uri = if connection.auth_type == "oauth2_authorization_code" {
                        Some(self.oauth()?.redirect_uri().to_owned())
                    } else {
                        None
                    };
                    serde_json::to_value(ConnectionGetResult {
                        connection,
                        redirect_uri,
                    })
                } else {
                    serde_json::to_value(connection)
                }
            }
            Outcome::List(details) => serde_json::to_value(ConnectionListResult {
                connections: details
                    .into_iter()
                    .map(|details| summary(details, self.org, &target.project, env))
                    .collect(),
            }),
            Outcome::Authorize(row) => {
                let oauth = self.oauth()?;
                let local;
                let envelope = if let Some(envelope) = oauth.shared_envelope() {
                    envelope
                } else {
                    local = self.logins.oauth_key(&namespace).await?;
                    &local
                };
                let authorization = oauth
                    .prepare_authorization(envelope, &row, &target.project, env)
                    .await
                    .map_err(|error| Refusal::Failed(error.into()))?;
                let mut project = client.begin().await?;
                bind_actor(&mut project, attachment, caller).await?;
                let authorization_url = authorization
                    .store(&mut project, *caller)
                    .await
                    .map_err(|error| Refusal::Failed(error.into()))?;
                project.commit().await?;
                serde_json::to_value(ConnectionAuthorizeResult { authorization_url })
            }
            Outcome::Other(mut value) => {
                if let Some(credentials) = value
                    .as_object_mut()
                    .and_then(|row| row.remove("credentials"))
                {
                    // Repeating deactivation can read the same immutable references.
                    destroy_credentials(&credentials_store, &credentials).await?;
                }
                Ok(value)
            }
        };
        Ok(value.map_err(anyhow::Error::from)?)
    }

    fn oauth(&self) -> Result<&super::oauth_authorization::Authorization, Refusal> {
        self.oauth
            .ok_or_else(|| Refusal::Failed(anyhow::anyhow!("OAuth authorization unavailable")))
    }
}

async fn bind_actor(
    project: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    attachment: &HostAttachment,
    caller: &PrincipalId,
) -> Result<(), Refusal> {
    wamn_postgres::bind_transaction_identity(
        project,
        caller,
        "",
        "",
        Some(attachment.operation.as_str()),
        None,
    )
    .await?;
    Ok(())
}

fn summary(
    details: ConnectionDetails,
    org: &wamn_registry::OrgId,
    project: &ProjectId,
    env: &EnvName,
) -> ConnectionSummary {
    let ConnectionDetails {
        row,
        versions,
        deprecated_fields,
    } = details;
    let ConnectionRow {
        id,
        name,
        integration_type,
        auth_type,
        external_system_id,
        target_id,
        resource_key,
        authority,
        active_generation,
        service_principal_id,
        active,
        parameters,
        credential_created_at,
        credential_created_by,
        refresh_status,
        expires_at,
    } = row;
    ConnectionSummary {
        id: id.to_string(),
        name,
        org: org.to_string(),
        project: project.to_string(),
        env: env.to_string(),
        integration_type,
        auth_type,
        external_system_id: external_system_id.map(|id| id.to_string()),
        target_id: target_id.to_string(),
        resource_key,
        authority,
        active_generation,
        service_principal_id: service_principal_id.to_string(),
        status: if active { "active" } else { "disabled" }.to_owned(),
        parameters,
        credential_created_at,
        credential_created_by: credential_created_by.map(|id| id.to_string()),
        refresh_status,
        expires_at,
        versions,
        deprecated_fields,
    }
}

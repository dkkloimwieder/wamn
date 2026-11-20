//! Read-only candidate testing under the connection writer's authority.

use wamn_routes::HostAttachment;
use std::time::Duration;

use serde_json::{Value, json};
use wamn_environment::connections::administration;
use wamn_identity_protocol::PrincipalId;
use wamn_kubernetes::Store as _;

use super::{ControlRoutes, ProjectTarget, Refusal, parse};

impl ControlRoutes<'_> {
    pub(super) async fn connection_test(
        &self,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        match tokio::time::timeout(
            Duration::from_secs(10),
            self.connection_test_in(attachment, caller, payload),
        )
        .await
        {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error @ Refusal::Delivery(_))) => Err(error),
            Ok(Err(Refusal::Declared { code, detail })) => Ok(
                json!({"ok":false,"error":format!("{code}: {}", detail["field"].as_str().unwrap_or("value"))}),
            ),
            Ok(Err(Refusal::Failed(_))) => {
                Ok(json!({"ok":false,"error":"connection health unavailable"}))
            }
            Err(_) => Ok(json!({"ok":false,"error":"connection health timed out"})),
        }
    }

    async fn connection_test_in(
        &self,
        attachment: &HostAttachment,
        caller: &PrincipalId,
        payload: &str,
    ) -> Result<Value, Refusal> {
        let invalid = |field| Refusal::Declared {
            code: "invalid_input",
            detail: json!({"field":field}),
        };
        let mut request: Value = serde_json::from_str(payload).map_err(|_| invalid("value"))?;
        administration::validate_request("connection.test", &request)?;
        let credential = request
            .as_object_mut()
            .unwrap()
            .remove("credential")
            .filter(|value| !value.is_null());
        let project: wamn_registry::ProjectId = request["project"]
            .as_str()
            .unwrap()
            .parse()
            .map_err(|_| invalid("project"))?;
        let target: ProjectTarget = parse(payload)?;
        let env = target
            .env
            .as_ref()
            .expect("connection request requires an environment");
        let (context, pin) = {
            let mut writer = self.writer.lock().await?;
            let mut authorized = self
                .begin_in(&mut writer, attachment, caller, Some(&project))
                .await?;
            let pin = administration::required_connection_version(
                &mut authorized,
                self.org,
                env,
                request["name"].as_str().expect("validated name"),
            )
            .await?;
            authorized.commit().await?;
            let login = self.login(&project, env).await?;
            let mut client = wamn_postgres::connect(login).await?;
            let mut transaction =
                wamn_identity::transaction::read_connection(&mut client, caller).await?;
            let context = administration::configure(
                &mut transaction,
                &mut writer,
                self.org,
                "connection.test",
                &json!({"project":project,"env":env,"name":request["name"]}),
            )
            .await?;
            transaction.rollback().await?;
            (context, pin)
        };
        if matches!(context["runtime_type"].as_str(), Some("wasm" | "native")) {
            return Ok(
                json!({"ok":false,"error":"integration health unavailable outside an application workload"}),
            );
        }
        let schemas = context["schemas"]
            .as_array()
            .ok_or_else(|| invalid("version"))?;
        administration::validate_connection_parameters(
            &request["parameters"],
            context.get("parameters"),
            schemas,
            &context["auth_schema"],
        )?;
        let versions = context["versions"]
            .as_object()
            .ok_or_else(|| invalid("version"))?;
        let requested = request["version"]
            .as_i64()
            .and_then(|version| i32::try_from(version).ok());
        if let Some(pin) = pin.filter(|pin| requested.is_some_and(|requested| requested != *pin)) {
            let pinned = versions
                .get(&pin.to_string())
                .ok_or_else(|| invalid("version"))?;
            administration::validate_version_parameters(
                &request["parameters"],
                &pinned["parameter_schema"],
            )?;
        }
        let selected = requested
            .or(pin)
            .or_else(|| {
                versions
                    .values()
                    .filter_map(|version| version["version"].as_i64())
                    .max()
                    .and_then(|version| i32::try_from(version).ok())
            })
            .ok_or_else(|| invalid("version"))?;
        let version = versions
            .get(&selected.to_string())
            .ok_or_else(|| invalid("version"))?;
        administration::validate_version_parameters(
            &request["parameters"],
            &version["parameter_schema"],
        )?;
        let auth = context["auth_type"]
            .as_str()
            .ok_or_else(|| invalid("auth_type"))?;
        let credential = match credential {
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| invalid("credential"))?
                    .to_owned(),
            ),
            None if context["credential"].is_object() => {
                let stored = &context["credential"];
                let store = wamn_kubernetes::kubernetes::KubernetesStore::in_cluster()
                    .map_err(|_| invalid("credential"))?;
                let bytes = store
                    .get(
                        stored["ref"]
                            .as_str()
                            .ok_or_else(|| invalid("credential"))?,
                        stored["version"]
                            .as_str()
                            .ok_or_else(|| invalid("credential"))?,
                    )
                    .await
                    .map_err(|_| invalid("credential"))?;
                Some(String::from_utf8(bytes).map_err(|_| invalid("credential"))?)
            }
            None => None,
        };
        if let Some(value) = &credential {
            let value: Value = serde_json::from_str(value).map_err(|_| invalid("credential"))?;
            if auth == "none" || !administration::validates(&value, &context["credential_schema"]) {
                return Err(invalid("credential"));
            }
        } else if auth != "none" {
            return Err(invalid("credential"));
        }
        let health = self
            .connection_health
            .ok_or_else(|| invalid("integration_type"))?;
        let integration_type = context["integration_type"]
            .as_str()
            .ok_or_else(|| invalid("integration_type"))?
            .parse::<wamn_registry::integration_code::IntegrationCode>()
            .map_err(|_| invalid("integration_type"))?;
        let result = health
            .probe(
                &integration_type,
                context["runtime_type"]
                    .as_str()
                    .ok_or_else(|| invalid("integration_type"))?,
                &request["parameters"],
                auth,
                credential.as_deref(),
            )
            .await;
        Ok(match result {
            Ok(()) => json!({"ok":true}),
            Err(error) => json!({"ok":false,"error":error}),
        })
    }
}

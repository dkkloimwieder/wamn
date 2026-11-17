//! Write the committed integration documents of one environment before its
//! release apply binds them.
//!
//! `documents` holds, for each integration the environment declares,
//! `<name>.integration.json`, `<name>.json` (the connection) and, when the
//! integration names one, `<system>.external-system.json`. The writes use the
//! administration operations of the control routes, which start only at
//! 4.19.1. A write whose stored value is equal is skipped.

use sqlx::Connection as _;
use std::path::Path;

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_connection::connections::{ConnectionError, administration};

use super::Platform;
use super::document::EnvironmentDocument;

/// Returns one line per write. No line means every value was already stored.
pub async fn write(
    platform: &Platform,
    document: &EnvironmentDocument,
    documents: &Path,
) -> anyhow::Result<Vec<String>> {
    let lock = wamn_catalog::lifecycle_lock::LifecycleLock::acquire(
        wamn_postgres::connect(&platform.system_database_url).await?,
        &document.triple(),
    )
    .await?;
    let result = lock
        .protect(async {
            crate::provision_project_env::registry::require_connected(
                &mut *lock.client().await,
                &document.triple(),
            )
            .await?;
            let row=super::analyse::read_row(&mut *lock.client().await,document).await?.context("environment is not provisioned")?;
            let url=super::analyse::project_url(platform,document,&row).await?;
            let mut client=super::analyse::connect_project(&url).await?;
            let mut system=wamn_postgres::connect(&platform.system_database_url).await?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET ROLE {}", wamn_role::SYSTEM_ROLE))).execute(&mut system).await?;
            let mut tx=client.begin().await?;
            let caller=wamn_provision::PlatformComponent::Provisioning.principal_id();
            sqlx::query("INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1,'','','connection.write')").bind(caller).execute(&mut *tx).await?;
            let mut writes=Writes{tx:&mut tx,system:&mut system,org:&document.org,caller};
            let mut lines = Vec::new();
            for name in document.integrations.keys() {
                let integration = read(documents, &format!("{name}.integration.json"))?;
                let connection = read(documents, &format!("{name}.json"))?;
                anyhow::ensure!(
                    integration["name"] == name.as_str()
                        && connection["name"] == name.as_str()
                        && integration["project"] == document.project.as_str()
                        && connection["project"] == document.project.as_str()
                        && connection["env"] == document.env.as_str(),
                    "the documents of integration {name} name another integration or environment"
                );
                if let Some(system) = integration["external_system"].as_str() {
                    let mut external = read(documents, &format!("{system}.external-system.json"))?;
                    external["env"]=json!(document.env);
                    let listed = writes
                        .run(
                            "external_system.list",
                            &json!({"project": document.project,"env":document.env}),
                        )
                        .await?;
                    if !listed["external_systems"]
                        .as_array()
                        .is_some_and(|rows| rows.iter().any(|row| row["name"] == system))
                    {
                        writes.run("external_system.write", &external).await?;
                        lines.push(format!("wrote external system {system}"));
                    }
                }
                let key = json!({"project": document.project, "env": document.env, "name": name});
                let mut request = connection.clone();
                for field in ["integration_type","auth_type","external_system"] {
                    request[field]=integration[field].clone();
                }
                match writes.stored("connection.get", &key).await? {
                    Some(stored) if stored["parameters"] == connection["parameters"] => continue,
                    // A first write of an authenticated integration stores an empty
                    // credential, which `connection.set_credential` replaces.
                    None if integration["auth_type"] != "none" => {
                        request["credential"] = "{}".into();
                    }
                    Some(_) | None => {}
                }
                writes.run("connection.write", &request).await?;
                lines.push(format!("wrote connection {name}"));
            }
            tx.commit().await.context("commit the connection writes")?;
            Ok(lines)
        })
        .await;
    lock.release().await;
    result
}

fn read(documents: &Path, file: &str) -> anyhow::Result<Value> {
    let path = documents.join(file);
    serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))
}

struct Writes<'a, 'c> {
    tx: &'a mut sqlx::Transaction<'c, sqlx::Postgres>,
    org: &'a wamn_registry::OrgId,
    system: &'a mut sqlx::PgConnection,
    caller: wamn_identity_protocol::PrincipalId,
}

impl Writes<'_, '_> {
    async fn call(&mut self, operation: &str, request: &Value) -> Result<Value, ConnectionError> {
        administration::execute(
            self.tx,
            self.system,
            self.caller,
            self.org,
            operation,
            &request.to_string(),
            None,
            &[],
            crate::application::service::write_connection_service,
        )
        .await
    }

    async fn run(&mut self, operation: &str, request: &Value) -> anyhow::Result<Value> {
        self.call(operation, request)
            .await
            .with_context(|| format!("{operation} {request}"))
    }

    /// The stored row, or `None` when the operation finds none.
    async fn stored(&mut self, operation: &str, key: &Value) -> anyhow::Result<Option<Value>> {
        match self.call(operation, key).await {
            Ok(row) => Ok(Some(row)),
            Err(error) if error.code == "not_found" => Ok(None),
            Err(error) => Err(error).with_context(|| format!("{operation} {key}")),
        }
    }
}

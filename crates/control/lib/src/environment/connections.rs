//! Write the committed integration documents of one environment before its
//! release apply binds them (platform install step 4.16.1).
//!
//! `documents` holds, for each integration the environment declares,
//! `<name>.integration.json`, `<name>.json` (the connection) and, when the
//! integration names one, `<system>.external-system.json`. The writes use the
//! administration operations of the control routes, which start only at
//! 4.19.1. A write whose stored value is equal is skipped.

use std::path::Path;

use anyhow::Context as _;
use serde_json::{Value, json};
use tokio_postgres::Transaction;
use wamn_platform_identity::auth_changes::AuthDrops;
use wamn_platform_identity::connection_changes::{ConnectionChanges, ConnectionDrops};
use wamn_platform_identity::connections::{ConnectionError, administration};

use super::Platform;
use super::document::EnvironmentDocument;

/// Returns one line per write. No line means every value was already stored.
pub async fn write(
    platform: &Platform,
    document: &EnvironmentDocument,
    documents: &Path,
) -> anyhow::Result<Vec<String>> {
    let mut client = super::analyse::connect(&platform.system_database_url).await?;
    let tx = client
        .transaction()
        .await
        .context("begin the connection writes")?;
    tx.batch_execute("SET LOCAL ROLE wamn_system").await?;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    let mut writes = Writes {
        tx: &tx,
        org: &document.org,
        auth: AuthDrops::default(),
        drops: ConnectionDrops::default(),
    };
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
            let external = read(documents, &format!("{system}.external-system.json"))?;
            let listed = writes
                .run(
                    "external_system.list",
                    &json!({"project": document.project}),
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
        let key = json!({"project": document.project, "name": name});
        if writes.stored("integration.get", &key).await?.is_none() {
            writes.run("integration.write", &integration).await?;
            lines.push(format!("wrote integration {name}"));
        }
        let key = json!({"project": document.project, "env": document.env, "name": name});
        let mut request = connection.clone();
        match writes.stored("connection.get", &key).await? {
            Some(stored) if stored["parameters"] == connection["parameters"] => continue,
            // A first write of an authenticated integration stores an empty
            // credential, which `connection.set_credential` replaces.
            None if integration["auth_type"] != "none" => request["credential"] = "{}".into(),
            Some(_) | None => {}
        }
        writes.run("connection.write", &request).await?;
        lines.push(format!("wrote connection {name}"));
    }
    let Writes { auth, drops, .. } = writes;
    tx.commit().await.context("commit the connection writes")?;
    let changes = platform.auth_changes().await?;
    changes.publish(auth).await;
    ConnectionChanges::from(&changes).publish(drops).await;
    Ok(lines)
}

fn read(documents: &Path, file: &str) -> anyhow::Result<Value> {
    let path = documents.join(file);
    serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))
}

struct Writes<'a> {
    tx: &'a Transaction<'a>,
    org: &'a str,
    auth: AuthDrops,
    drops: ConnectionDrops,
}

impl Writes<'_> {
    async fn call(&mut self, operation: &str, request: &Value) -> Result<Value, ConnectionError> {
        administration::execute(
            self.tx,
            self.org,
            operation,
            &request.to_string(),
            &mut self.auth,
            &mut self.drops,
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

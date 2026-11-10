//! Deployment operations through the authenticated control host.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use serde_json::{Value, json};
use wamn_environment_control::environment::document::EnvironmentDocument;

pub(crate) struct ControlHost {
    url: String,
    token: String,
}

impl ControlHost {
    pub(crate) fn from_env() -> anyhow::Result<Option<Self>> {
        let url = match std::env::var("WAMN_CONTROL_URL") {
            Ok(url) => url,
            Err(std::env::VarError::NotPresent) => return Ok(None),
            Err(error) => return Err(error).context("read WAMN_CONTROL_URL"),
        };
        ensure!(!url.trim().is_empty(), "WAMN_CONTROL_URL is empty");
        let token = std::env::var("WAMN_TOKEN").context("set WAMN_TOKEN for the control host")?;
        ensure!(!token.trim().is_empty(), "WAMN_TOKEN is empty");
        Ok(Some(Self { url, token }))
    }

    pub(crate) async fn run(
        &self,
        operation: &str,
        file: &Path,
        mut value: Value,
    ) -> anyhow::Result<()> {
        let document = EnvironmentDocument::compile(file)?;
        value["project"] = document.project.into();
        value["env"] = document.env.into();
        let client = reqwest::Client::new();
        let started =
            crate::component_verbs::invoke(&client, &self.url, &self.token, operation, &value)
                .await?;
        let job = started["job"].as_str().context("operation Job name")?;
        let target = json!({"project":value["project"], "env":value["env"], "job":job});
        let mut previous = Value::Null;
        loop {
            let mut status = crate::component_verbs::invoke(
                &client,
                &self.url,
                &self.token,
                "operation.status",
                &target,
            )
            .await?;
            let state = status["state"].take();
            let name = state.as_str().context("operation state")?;
            if state != previous {
                println!("{}", json!({"job":job, "state":name}));
            }
            match name {
                "succeeded" | "failed" => {
                    println!("{}", status["log"].as_str().context("operation log tail")?);
                    ensure!(name == "succeeded", "{operation} Job {job} failed");
                    return Ok(());
                }
                "pending" | "running" => {
                    previous = state;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                state => bail!("{operation} Job {job} returned unknown state {state}"),
            }
        }
    }
}

/// Publish source with registry credentials and disposable build databases only.
pub(crate) async fn publish(
    document: &EnvironmentDocument,
    package: &Path,
) -> anyhow::Result<(String, wamn_catalog::ManifestDigest)> {
    let database_url =
        std::env::var("DATABASE_URL").context("set DATABASE_URL for disposable build databases")?;
    wamn_dev_control::source_deploy::publish_from_env(document, package, &database_url).await
}

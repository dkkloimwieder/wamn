//! Deployment operations through the authenticated control host.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use serde_json::{Value, json};
use wamn_environment::environment::document::EnvironmentDocument;

// The Job deadline is fifteen minutes. Allow time for scheduling and status delivery.
const POLL_LIMIT: Duration = Duration::from_mins(16);

pub(crate) struct ControlHost {
    url: String,
    token: String,
}

impl ControlHost {
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        let url = std::env::var("WAMN_CONTROL_URL").context("set WAMN_CONTROL_URL")?;
        ensure!(!url.trim().is_empty(), "WAMN_CONTROL_URL is empty");
        let token = std::env::var("WAMN_TOKEN").context("set WAMN_TOKEN for the control host")?;
        ensure!(!token.trim().is_empty(), "WAMN_TOKEN is empty");
        Ok(Self { url, token })
    }

    pub(crate) async fn call(&self, operation: &str, value: &Value) -> anyhow::Result<Value> {
        crate::component_verbs::invoke(&self.url, &self.token, operation, value).await
    }

    pub(crate) async fn run(
        &self,
        operation: &str,
        coordinate: &wamn_registry::Triple,
        value: Value,
    ) -> anyhow::Result<()> {
        self.project(operation, &coordinate.project, &coordinate.env, value)
            .await
    }

    pub(crate) async fn project(
        &self,
        operation: &str,
        project: &wamn_registry::ProjectId,
        env: &wamn_registry::EnvName,
        mut value: Value,
    ) -> anyhow::Result<()> {
        value["project"] = serde_json::json!(project);
        value["env"] = serde_json::json!(env);
        self.run_value(operation, value, "operation.status").await
    }

    pub(crate) async fn platform_schema(&self, value: Value) -> anyhow::Result<()> {
        self.run_value("platform.schema", value, "platform.operation_status")
            .await
    }

    pub(crate) async fn environment(
        &self,
        operation: &str,
        triple: &wamn_registry::Triple,
        mut value: Value,
    ) -> anyhow::Result<()> {
        value["project"] = triple.project.as_str().into();
        value["env"] = serde_json::json!(&triple.env);
        self.run_value(operation, value, "environment.operation_status")
            .await
    }

    async fn run_value(
        &self,
        operation: &str,
        value: Value,
        status_operation: &str,
    ) -> anyhow::Result<()> {
        let started =
            crate::component_verbs::invoke(&self.url, &self.token, operation, &value).await?;
        let job = started["job"].as_str().context("operation Job name")?;
        let mut target = json!({"job":job});
        for field in ["project", "env"] {
            if let Some(value) = value.get(field) {
                target[field] = value.clone();
            }
        }
        tokio::time::timeout(POLL_LIMIT, async {
            let mut previous = Value::Null;
            loop {
                let mut status = crate::component_verbs::invoke(
                    &self.url,
                    &self.token,
                    status_operation,
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
        })
        .await
        .with_context(|| format!("timed out waiting for {operation} Job {job}"))?
    }
}

/// Publish source with registry credentials and committed describe evidence.
pub(crate) async fn publish(
    document: &EnvironmentDocument,
    package: &Path,
) -> anyhow::Result<(wamn_registry::ApplicationName, wamn_digest::Digest)> {
    wamn_build::source_deploy::publish_from_env(
        &wamn_build::source_build::SourceBuilder {
            describe: Ok(wamn_build::DescribeMode::Cached),
        },
        &crate::package_verbs::package_publisher(),
        document,
        package,
    )
    .await
}

/// Parse the existing org/env/project CLI coordinate.
pub(crate) fn parse_coordinate(value: &str) -> anyhow::Result<wamn_registry::Triple> {
    let parts: Vec<_> = value.split('/').collect();
    let [org, env, project] = parts.as_slice() else {
        anyhow::bail!("a coordinate is org/env/project");
    };
    let org = org.parse()?;
    let project = project.parse()?;
    let env = env.parse()?;
    wamn_environment::name::validate_project_env(&org, &project, &env)?;
    Ok(wamn_registry::Triple::new(org, env, project))
}

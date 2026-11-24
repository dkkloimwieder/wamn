//! Apply and observe the environment releases in Kubernetes.

use anyhow::Context as _;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::time::{Instant, timeout_at};

use crate::{
    Target,
    helm::{self, Install, Wait},
};

/// Recover the host release and prepare one upgrade that waits for readiness.
pub async fn prepare_host_upgrade<'a>(
    target: &'a Target,
    chart: &Path,
    name: String,
    values: &Value,
) -> anyhow::Result<impl Future<Output = anyhow::Result<()>> + use<'a>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("values.yaml");
    std::fs::write(&path, serde_json::to_vec(values)?)?;
    let chart = chart.join("platform/environment");
    let namespace = helm::org_release_namespace(&target.namespace);
    helm::recover_pending(target, &namespace, &name, Wait::Watcher).await?;
    Ok(async move {
        let _directory = directory;
        helm::upgrade(
            target,
            &namespace,
            &chart,
            &name,
            &[&path],
            Wait::Watcher,
            Install::SkipCrds,
        )
        .await
        .with_context(|| format!("upgrade environment host {name}"))
    })
}

/// Upgrade one application with SQL and workflow checks already complete.
pub async fn apply_application(
    target: &Target,
    chart: &Path,
    name: &str,
    values: &serde_yaml::Value,
    deadline: Instant,
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("application.yaml");
    std::fs::write(&path, serde_yaml::to_string(values)?)?;
    let namespace = helm::org_release_namespace(&target.namespace);
    timeout_at(deadline, async {
        helm::recover_pending(target, &namespace, name, Wait::Watcher).await?;
        helm::upgrade(
            target,
            &namespace,
            &chart.join("platform/application"),
            name,
            &[&path],
            Wait::WatcherUntil(deadline),
            Install::SkipCrds,
        )
        .await
    })
    .await
    .with_context(|| {
        format!("deploy Job deadline reached applying Helm release {namespace}/{name}")
    })?
}

/// Submit the project resources before the worker initializes the database and CDC checkpoint.
pub async fn apply_project(
    target: &Target,
    chart: &Path,
    name: &str,
    values: &Value,
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("values.yaml");
    std::fs::write(&path, serde_json::to_vec(values)?)?;
    let namespace = helm::org_release_namespace(&target.namespace);
    helm::recover_pending(target, &namespace, name, Wait::None).await?;
    helm::upgrade(
        target,
        &namespace,
        &chart.join("platform/project"),
        name,
        &[&path],
        Wait::None,
        Install::ExistingNamespace,
    )
    .await
}

pub fn entry(chart: &Path, namespace: &str, name: &str, values: Value) -> anyhow::Result<Value> {
    let metadata: Value = serde_yaml::from_slice(&std::fs::read(chart.join("Chart.yaml"))?)?;
    let mut entry =
        json!({"chart":chart,"name":name,"namespace":namespace,"version":metadata["version"]});
    entry["values"] = values;
    Ok(entry)
}

pub fn resource(objects: Vec<Value>, type_: &str) -> anyhow::Result<Value> {
    objects
        .into_iter()
        .find(|object| object["kind"] == type_)
        .context("chart did not render the requested resource")
}

/// Wait for CloudNativePG to apply the current generation of a named database.
///
/// # Errors
/// Names the Database if Kubernetes cannot read it or it stays unapplied.
pub async fn wait_database_applied(
    target: &Target,
    database: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    use crate::client::{resource, wait_named};
    let databases = resource(
        target.client.client(),
        &target.namespace,
        "postgresql.cnpg.io",
        "v1",
        "Database",
        "databases",
    );
    wait_named(&databases, database, timeout, |object| {
        Ok(object
            .filter(|object| {
                object.data["status"]["applied"] == true
                    && object.metadata.generation.is_some_and(|generation| {
                        object.data["status"]["observedGeneration"].as_i64() == Some(generation)
                    })
            })
            .map(drop))
    })
    .await
}

/// Set the lifecycle values of an installed project.
pub async fn update_project(
    target: &Target,
    chart: &Path,
    name: &str,
    mut values: Value,
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("project-values.yaml");
    values["namespace"] = target.namespace.as_str().into();
    std::fs::write(&path, serde_json::to_vec(&values)?)?;
    let namespace = helm::org_release_namespace(&target.namespace);
    helm::recover_pending(target, &namespace, name, Wait::None).await?;
    helm::update_values(
        target,
        &namespace,
        &chart.join("platform/project"),
        name,
        &path,
    )
    .await
}

/// Uninstall the project release. Storage and its ObjectStore are kept by default.
pub async fn remove_database(
    target: &Target,
    chart: &Path,
    name: &str,
    data: bool,
) -> anyhow::Result<()> {
    if data {
        update_project(target, chart, name, json!({"deleteData":true})).await?;
    }
    let namespace = helm::org_release_namespace(&target.namespace);
    helm::uninstall(target, &namespace, name, Wait::None).await
}

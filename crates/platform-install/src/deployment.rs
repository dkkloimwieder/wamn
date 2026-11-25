//! Apply and observe the environment releases in Kubernetes.

use anyhow::Context as _;
use serde_json::{Value, json};
use std::{path::Path, pin::Pin, time::Duration};
use tokio::time::{Instant, timeout_at};
use wamn_kubernetes::{
    Target,
    helm::{self, Install, Wait},
};
use wamn_postgres::database::ConnectionConfig;
use wamn_registry::{Triple, name};

/// Recover the host release and prepare one upgrade that waits for readiness.
pub async fn prepare_host_upgrade<'a>(
    target: &'a Target,
    chart: &Path,
    name: String,
    values: &Value,
) -> anyhow::Result<Upgrade<'a>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("values.yaml");
    std::fs::write(&path, serde_json::to_vec(values)?)?;
    let chart = chart.join("platform/environment");
    let namespace = helm::org_release_namespace(&target.namespace);
    helm::recover_pending(target, &namespace, &name, Wait::Watcher).await?;
    Ok(Box::pin(async move {
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
    }))
}

/// Upgrade one application after release recovery, SQL and workflow checks.
/// Helm reserves time for rollback within the deploy Job deadline.
pub async fn apply_application(
    target: &Target,
    chart: &Path,
    name: &str,
    values: &serde_yaml::Value,
    deadline: Instant,
    readiness: Duration,
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("application.yaml");
    std::fs::write(&path, serde_yaml::to_string(values)?)?;
    let namespace = helm::org_release_namespace(&target.namespace);
    timeout_at(deadline, async {
        let wait = Wait::WatcherUntil {
            deadline,
            readiness,
        };
        helm::upgrade(
            target,
            &namespace,
            &chart.join("platform/application"),
            name,
            &[&path],
            wait,
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

/// The pending host upgrade after preparation completes.
pub type Upgrade<'a> = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>>;

/// Read the host's stamped image from the selected chart.
pub async fn host_image(chart: &Path) -> anyhow::Result<String> {
    Ok(crate::release_chart::image_set(chart).await?.host)
}

pub async fn wait_database(
    target: &Target,
    owner: &ConnectionConfig,
    capture: &ConnectionConfig,
) -> anyhow::Result<()> {
    let cluster = name::project_cluster(&owner.database);
    let cluster_uid = target.wait_database_cluster_uid(&cluster).await?;
    wamn_kubernetes::deployment_components::copy_server_ca(
        target,
        &cluster,
        &owner.server_ca.name,
        cluster_uid,
    )
    .await?;
    wamn_kubernetes::platform_environment::wait_database_applied(
        target,
        &owner.database,
        Duration::from_secs(120),
    )
    .await?;

    target
        .wait_database_certificates(&owner.credentials.name, &capture.credentials.name)
        .await
}

pub async fn delete_reader(
    target: &Target,
    chart: &Path,
    triple: &Triple,
    release: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(release) = release
        && target.has_reader_deployment(triple).await?
    {
        update_project(
            target, chart, release,
            serde_json::json!({"cdcReader":{"pause":true},"publication":{"reclaimPolicy":"delete"}}),
        ).await?;
    }
    target.wait_reader_stopped(triple).await
}

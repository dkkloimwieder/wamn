//! Helm owns environment resources; credential writes and SQL remain explicit.
use anyhow::Context as _;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;
use wamn_kubernetes::{
    Target,
    helm::{self, Install, Wait},
};
use wamn_provision::migrations::MigrationDatabase;
use wamn_registry::Triple;

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
            &path,
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
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("application.yaml");
    std::fs::write(&path, serde_yaml::to_string(values)?)?;
    let namespace = helm::org_release_namespace(&target.namespace);
    helm::upgrade(
        target,
        &namespace,
        &chart.join("platform/application"),
        name,
        &path,
        Wait::Watcher,
        Install::SkipCrds,
    )
    .await
}

/// Submit the project resources before the worker initializes the database and CDC slot.
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
        &path,
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

/// Set worker and credential values on the project release.
///
/// # Errors
/// Returns an error when the coordinates cannot name an environment resource.
pub fn set_operation_values<'a>(
    values: &mut Value,
    triple: &Triple,
    cluster: &str,
    database: &str,
    applications: impl Iterator<Item = &'a wamn_catalog::ApplicationName>,
) -> anyhow::Result<()> {
    let name = wamn_provision::environment_names::environment_resource_name(
        &triple.org,
        &triple.project,
        &triple.env,
    )?;
    let capture = wamn_provision::cdc_reader_name(&triple.org, &triple.project, &triple.env);
    let capture_secret = wamn_registry::name::project_login_secret(database, wamn_role::CDC_LOGIN);
    let diff_workloads: Vec<_> = applications
        .map(|application| {
            let release = wamn_catalog::resource_names::application_release_name(
                &triple.org,
                &triple.project,
                &triple.env,
                application,
            );
            wamn_registry::name::resource_suffix(&release, "http")
        })
        .collect();
    let diff_secrets = [
        MigrationDatabase::System.managed_role("wamn_system").password_secret.name,
        "wamn-pg-ca".into(),
        MigrationDatabase::Project.managed_role(database).password_secret.name,
        format!("{cluster}-ca"),
    ];
    values["operations"] = json!({
        "name":name,"cluster":cluster,
        "capture":capture,"captureSecret":capture_secret,
        "hostGroup":wamn_registry::name::environment_host_name(&triple.org, &triple.env),
        "controlDeployment":wamn_registry::name::control_resource(&triple.org),
        "diffWorkloads":diff_workloads,
        "diffSecrets":diff_secrets,
    });
    values["credentials"] = json!({
        "name":wamn_registry::credential_secret_name(&triple.org, &triple.project, &triple.env),
        "controlAccount":wamn_registry::name::control_service_account(&triple.org),
    });
    Ok(())
}

pub fn publication_values(
    triple: &wamn_registry::Triple,
    instance: &wamn_registry::instance_suffix::InstanceSuffix,
    cluster: &str,
    name: &str,
    schemas: &[wamn_catalog::canonical_name::CanonicalName],
) -> Value {
    json!({"publication":{"name":name,"cluster":cluster,"org":triple.org,"project":triple.project,"env":triple.env,
        "database":wamn_provision::project_env_database_name(&triple.org,&triple.project,&triple.env,instance),
        "schemas":schemas,"publish":wamn_provision::sql::CDC_PUBLISH}})
}

/// Wait for the project database to accept an authenticated SQL connection.
///
/// # Errors
/// Names the database if it cannot be reached within 120 seconds.
pub async fn wait_project_connection(
    input: &str,
    database: &str,
) -> anyhow::Result<sqlx::PgConnection> {
    let timeout = Duration::from_secs(120);
    let mut last_error = None;
    let connection = crate::credentials::connect_with_retry(input, |error| {
        last_error = Some(error);
        Ok(())
    });
    match tokio::time::timeout(timeout, connection).await {
        Ok(result) => result,
        Err(elapsed) => Err(last_error.unwrap_or_else(|| elapsed.into())),
    }
    .with_context(|| {
        format!(
            "project database {database} did not accept connections within {} seconds",
            timeout.as_secs()
        )
    })
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
    wamn_kubernetes::kubectl::wait_named(target, "database", database, timeout, |object| {
        object["status"]["applied"] == true
            && object["metadata"]["generation"].is_u64()
            && object["status"]["observedGeneration"] == object["metadata"]["generation"]
    })
    .await
}

/// Resolve the recorded database on its project's CNPG cluster.
pub async fn connection(
    registry: &str,
    triple: &wamn_registry::Triple,
) -> anyhow::Result<wamn_postgres::ConnectionConfig> {
    let mut client = wamn_postgres::connect(registry).await?;
    let instance = wamn_registry::deployment::instance_suffix(&mut client, triple)
        .await?
        .with_context(|| format!("project deployment {triple} is absent"))?;
    Ok(connection_for(triple, &instance))
}

/// Resolve a project endpoint from a generation already read from the registry.
pub fn connection_for(
    triple: &wamn_registry::Triple,
    instance: &wamn_registry::instance_suffix::InstanceSuffix,
) -> wamn_postgres::ConnectionConfig {
    use wamn_registry::name;
    let database =
        name::project_env_database_name(&triple.org, &triple.project, &triple.env, instance);
    let owner = wamn_provision::migrations::MigrationDatabase::Project.managed_role(&database);
    crate::credentials::configuration(
        triple.org.as_str(),
        &name::project_cluster(&database),
        name::project_database_host(&database, &triple.org),
        5432,
        database,
        owner.name,
        owner.password_secret.name,
    )
}

/// Derive the project capture credential from the same endpoint as its owner.
pub fn capture_configuration(
    mut config: wamn_postgres::ConnectionConfig,
) -> anyhow::Result<wamn_postgres::ConnectionConfig> {
    config.username = wamn_role::CDC_LOGIN.parse()?;
    config.credentials.name =
        wamn_registry::name::project_login_secret(&config.database, config.username.as_str());
    Ok(config)
}

/// Project sizing and role values for the chart.
pub fn database_values(
    connection: &wamn_postgres::ConnectionConfig,
    replicas: u32,
    mut sizing: Value,
    settings: &Value,
) -> anyhow::Result<Value> {
    use wamn_registry::name;
    let database = &connection.database;
    let cluster = name::project_cluster(database);
    let roles = crate::roles::project(database, replicas)?;
    let max_connections = roles
        .iter()
        .filter_map(|role| role.connection_limit)
        .sum::<u32>()
        + 10;
    if let Some(backup) = sizing.get_mut("backup") {
        for key in ["endpoint", "bucket", "secret"] {
            backup[key] = settings["backup"][key]
                .as_str()
                .filter(|value| !value.is_empty())
                .with_context(|| format!("installation PostgreSQL backup {key} is absent"))?
                .into();
        }
    }
    sizing["name"] = json!(cluster);
    sizing["image"] = settings["image"]
        .as_str()
        .context("installation PostgreSQL image is absent")?
        .into();
    sizing["roles"] = serde_json::to_value(roles)?;
    // Reserve headroom for PostgreSQL and the operator.
    sizing["maxConnections"] = max_connections.max(100).into();
    sizing["initializeRoles"] = wamn_provision::sql::ensure_project_roles_sql().into();
    sizing["ownerSecret"] = json!(connection.credentials.name);
    let extensions = wamn_provision::sql::PLATFORM_EXTENSIONS.map(|name| json!({"name":name}));
    Ok(json!({
        "namespace":connection.credentials.namespace,
        "cluster":sizing,
        "database":{
            "name":database,"cluster":cluster,"owner":connection.username,"extensions":extensions
        }
    }))
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
    triple: &wamn_registry::Triple,
    data: bool,
) -> anyhow::Result<()> {
    let name = wamn_provision::environment_names::environment_resource_name(
        &triple.org,
        &triple.project,
        &triple.env,
    )?;
    if data {
        update_project(target, chart, &name, json!({"deleteData":true})).await?;
    }
    let namespace = helm::org_release_namespace(&target.namespace);
    helm::uninstall(target, &namespace, &name, Wait::None).await
}

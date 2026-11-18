//! Helm owns environment resources; credential writes and SQL remain explicit.
use anyhow::Context as _;
use serde_json::{Value, json};
use std::path::Path;
use wamn_kubernetes::Target;

pub async fn apply(target: &Target, chart: &Path, name: &str, values: Value) -> anyhow::Result<()> {
    let entry = entry(chart, &target.namespace, name, values)?;
    let directory = tempfile::tempdir()?;
    crate::platform::resources::apply_release(target, &entry, directory.path()).await
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
    wamn_kubernetes::helm::upgrade_application(
        target,
        &chart.join("platform/application"),
        name,
        &path,
        std::time::Duration::from_secs(600),
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
    apply_without_wait(target, &chart.join("platform/project"), name, values).await
}

/// Submit one release while its workload still needs provisioning work.
pub async fn apply_without_wait(
    target: &Target,
    chart: &Path,
    name: &str,
    values: &Value,
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("values.yaml");
    std::fs::write(&path, serde_json::to_vec(values)?)?;
    wamn_kubernetes::helm::recover_pending(target, name).await?;
    wamn_kubernetes::helm::upgrade_without_wait(
        target,
        chart,
        name,
        &path,
        std::time::Duration::from_secs(600),
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

/// Resolve the recorded database on its owning CNPG cluster without reading credentials.
pub async fn connection(
    registry: &str,
    triple: &wamn_registry::Triple,
) -> anyhow::Result<wamn_postgres::ConnectionConfig> {
    let mut client = wamn_postgres::connect(registry).await?;
    let placement = wamn_registry::deployment::database_placement(&mut client, triple).await?;
    let policy = sqlx::query(sqlx::AssertSqlSafe(
        wamn_registry::sql::select_env_policy_sql(),
    ))
    .bind(&triple.org)
    .bind(&placement.policy_name)
    .fetch_one(&mut client)
    .await?;
    let policy = wamn_provision::env_policies::env_policy_from_row(&policy)?;
    let instance = placement.instance_suffix;
    let placement = match placement.placement_type.as_str() {
        "pooled" => wamn_provision::Placement::Pooled {
            pool: placement
                .pool_cluster
                .context("registered pool is absent")?,
        },
        "dedicated" => wamn_provision::Placement::Dedicated,
        _ => anyhow::bail!("unknown org placement"),
    };
    let org = wamn_provision::Org {
        id: triple.org.clone(),
        placement,
    };
    let cluster = wamn_provision::cluster_of(&org, &policy).name;
    let database = wamn_provision::project_env_database_name(
        &triple.org,
        &triple.project,
        &triple.env,
        &instance,
    );
    let owner = wamn_provision::migrations::MigrationDatabase::Project.managed_role(&database);
    Ok(crate::credentials::configuration(
        triple.org.as_str(),
        &cluster,
        format!("{cluster}-rw.{}.svc.cluster.local", triple.org),
        5432,
        database,
        owner.name,
        owner.password_secret.name,
    ))
}

/// A capture login owns slot administration, independently of SQL schema ownership.
pub async fn capture_connection(
    registry: &str,
    triple: &wamn_registry::Triple,
) -> anyhow::Result<String> {
    let mut config = connection(registry, triple).await?;
    let mut client = wamn_postgres::connect(registry).await?;
    let instance = wamn_registry::deployment::instance_suffix(&mut client, triple)
        .await?
        .context("project deployment is absent")?;
    config.username =
        wamn_provision::cdc_object_name(&triple.org, &triple.project, &triple.env, &instance)
            .parse()?;
    config.credentials.name =
        wamn_provision::project_env_cdc_secret_name(&triple.org, &triple.project, &triple.env);
    config.to_json()
}

/// Set the lifecycle values of an installed project.
pub async fn update_project(
    target: &Target,
    chart: &Path,
    name: &str,
    values: &Value,
) -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("project-values.yaml");
    std::fs::write(&path, serde_json::to_vec(values)?)?;
    wamn_kubernetes::helm::recover_pending(target, name).await?;
    wamn_kubernetes::helm::update_values(target, &chart.join("platform/project"), name, &path).await
}

/// The chart, rather than a direct CR patch, owns database disposal.
pub async fn remove_database(
    target: &Target,
    chart: &Path,
    registry: &str,
    triple: &wamn_registry::Triple,
    data: bool,
) -> anyhow::Result<()> {
    let name = wamn_provision::environment_names::environment_resource_name(
        &triple.org,
        &triple.project,
        &triple.env,
    )?;
    let connection = connection(registry, triple).await?;
    let cluster = connection
        .server_ca
        .name
        .strip_suffix("-ca")
        .context("project cluster CA")?;
    update_project(target, chart, &name,
        &json!({"database":{"name":connection.database,"cluster":cluster,"owner":wamn_provision::migration::login(&connection.database),"ensure":"absent","reclaimPolicy":if data {"delete"} else {"retain"}}})).await?;
    wamn_kubernetes::kubectl::wait(
        &target.kubeconfig,
        &target.context,
        triple.org.as_str(),
        "database",
        &connection.database,
        "condition=Applied",
        "600s",
    )
    .await?;
    wamn_kubernetes::helm::uninstall(target, &name).await
}

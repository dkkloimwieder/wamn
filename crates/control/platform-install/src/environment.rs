//! Helm owns environment resources; credential writes and SQL remain explicit.
use anyhow::Context as _;
use serde_json::{Value, json};
use std::path::Path;
use wamn_release_control::release_chart::Target;

pub async fn apply(
    target: &Target,
    chart: &Path,
    name: &str,
    overlay: Value,
) -> anyhow::Result<()> {
    let mut values = crate::platform::installed::release_values(target, &target.namespace, name)
        .await?
        .unwrap_or_else(|| json!({}));
    crate::platform::resources::merge(&mut values, overlay);
    let entry = entry(chart, &target.namespace, name, values)?;
    let directory = tempfile::tempdir()?;
    crate::platform::resources::apply_release(target, &entry, directory.path()).await
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
    instance: &str,
    cluster: &str,
    name: &str,
    schemas: &[String],
) -> Value {
    json!({"publication":{"name":name,"cluster":cluster,"org":triple.org,"project":triple.project,"env":triple.env,
        "database":wamn_control_provision::project_env_database_name(&triple.org,&triple.project,triple.env.as_str(),instance),
        "schemas":schemas,"publish":wamn_control_provision::sql::CDC_PUBLISH}})
}

/// Keep broker-owned certificate parameters in their Helm owner's values.
pub fn broker_certificates(certificates: Vec<Value>) -> Value {
    json!({"brokerCertificates":certificates.into_iter().map(|mut certificate| {
        let mut entry = json!({});
        entry["name"] = certificate["metadata"]["name"].take();
        entry["spec"] = certificate["spec"].take();
        entry
    }).collect::<Vec<_>>()})
}

/// Resolve the recorded database on its owning CNPG cluster without reading credentials.
pub async fn connection(
    registry: &str,
    triple: &wamn_registry::Triple,
) -> anyhow::Result<wamn_postgres::ConnectionConfig> {
    use sqlx::Row as _;
    let mut client = wamn_postgres::connect(registry).await?;
    let row = sqlx::query("SELECT ep.instance_suffix, o.placement_type, o.pool_cluster, e.policy_name FROM registry.environment_projects ep JOIN registry.orgs o ON o.id=ep.org JOIN registry.environments e USING(org,env) WHERE ep.org=$1 AND ep.project=$2 AND ep.env=$3")
        .bind(&triple.org).bind(&triple.project).bind(triple.env.as_str()).fetch_one(&mut client).await?;
    let instance: String = row.try_get(0)?;
    let placement: String = row.try_get(1)?;
    let pool: Option<String> = row.try_get(2)?;
    let policy_name: String = row.try_get(3)?;
    let policy = sqlx::query(sqlx::AssertSqlSafe(
        wamn_registry::sql::select_env_policy_sql(),
    ))
    .bind(&triple.org)
    .bind(&policy_name)
    .fetch_one(&mut client)
    .await?;
    let policy = wamn_release_control::env_policies::env_policy_from_row(&policy)?;
    let placement = match placement.as_str() {
        "pooled" => wamn_control_provision::Placement::Pooled {
            pool: pool.context("registered pool is absent")?,
        },
        "dedicated" => wamn_control_provision::Placement::Dedicated,
        _ => anyhow::bail!("unknown org placement"),
    };
    let org = wamn_control_provision::Org {
        id: triple.org.clone(),
        placement,
    };
    let cluster = wamn_control_provision::cluster_of(&org, &policy).name;
    Ok(crate::credentials::configuration(
        &triple.org,
        &cluster,
        format!("{cluster}-rw.{}.svc.cluster.local", triple.org),
        5432,
        wamn_control_provision::project_env_database_name(
            &triple.org,
            &triple.project,
            &triple.env,
            &instance,
        ),
        "wamn_apply".into(),
        "wamn-apply".into(),
    ))
}

/// A capture login owns slot administration, independently of SQL schema ownership.
pub async fn capture_connection(
    registry: &str,
    triple: &wamn_registry::Triple,
) -> anyhow::Result<String> {
    let mut config = connection(registry, triple).await?;
    let mut client = wamn_postgres::connect(registry).await?;
    let instance: String = sqlx::query_scalar("SELECT instance_suffix FROM registry.environment_projects WHERE org=$1 AND project=$2 AND env=$3")
        .bind(&triple.org).bind(&triple.project).bind(triple.env.as_str()).fetch_one(&mut client).await?;
    config.username = wamn_control_provision::cdc_object_name(
        &triple.org,
        &triple.project,
        &triple.env,
        &instance,
    );
    config.credentials.name = wamn_control_provision::project_env_cdc_secret_name(
        &triple.org,
        &triple.project,
        &triple.env,
    );
    config.to_json()
}

/// Apply the registry-derived role declaration before CNPG creates the database.
pub async fn provision(
    target: &Target,
    chart: &Path,
    registry: &str,
    triple: &wamn_registry::Triple,
    initial: bool,
) -> anyhow::Result<wamn_postgres::ConnectionConfig> {
    let connection = connection(registry, triple).await?;
    let cluster = connection
        .server_ca
        .name
        .strip_suffix("-ca")
        .context("CNPG cluster CA")?;
    let role = wamn_control_provision::migrations::MigrationDatabase::Project
        .managed_role(&connection.database);
    crate::credentials::reconcile(
        registry,
        &target.kubeconfig,
        &target.context,
        &triple.org,
        cluster,
        &role,
    )
    .await?;
    let name = wamn_control_provision::environment_names::environment_resource_name(
        &triple.org,
        &triple.project,
        &triple.env,
    )?;
    apply(
        target,
        &chart.join("platform/environment"),
        &name,
        json!({"database":{"name":connection.database,"cluster":cluster},"bootstrap":if initial { json!({"name":format!("bootstrap-{}",&wamn_digest::bytes_sha256(connection.database.as_bytes())[7..27]),"credential":format!("{cluster}-superuser"),"ca":connection.server_ca.name}) } else {Value::Null}}),
    )
    .await?;
    wamn_kubernetes::kubectl::wait(
        &target.kubeconfig,
        &target.context,
        &triple.org,
        "database",
        &connection.database,
        "condition=Applied",
        "120s",
    )
    .await?;
    Ok(connection)
}

/// The chart, rather than a direct CR patch, owns database disposal.
pub async fn remove_database(
    target: &Target,
    chart: &Path,
    registry: &str,
    triple: &wamn_registry::Triple,
    data: bool,
) -> anyhow::Result<()> {
    let name = wamn_control_provision::environment_names::environment_resource_name(
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
    apply(target,&chart.join("platform/environment"),&name,
        json!({"database":{"name":connection.database,"cluster":cluster,"ensure":"absent","reclaimPolicy":if data {"delete"} else {"retain"}}})).await?;
    wamn_kubernetes::kubectl::wait(
        &target.kubeconfig,
        &target.context,
        &triple.org,
        "database",
        &connection.database,
        "condition=Applied",
        "600s",
    )
    .await?;
    wamn_release_control::release_chart::uninstall(target, &name).await
}

/// Initial SQL is an explicit installer bootstrap, never an update operation.
/// The restricted owner checks for the baseline before an administrator reference is built.
pub async fn initialize(
    target: &Target,
    image: &str,
    connection: &wamn_postgres::ConnectionConfig,
    allow_creation: bool,
) -> anyhow::Result<bool> {
    let owner = wamn_control_provision::migrations::MigrationDatabase::Project
        .configuration(&connection.to_json()?)?;
    let mut client = wamn_postgres::connect(&owner).await?;
    let present: bool = sqlx::query_scalar("SELECT to_regclass('platform.\"user\"') IS NOT NULL")
        .fetch_one(&mut client)
        .await?;
    if present {
        return Ok(false);
    }
    anyhow::ensure!(
        allow_creation,
        "configured environment has no SQL baseline; restore it from a trusted backup"
    );
    let cluster = connection
        .server_ca
        .name
        .strip_suffix("-ca")
        .context("project cluster CA")?;
    let mut bootstrap = connection.clone();
    bootstrap.username = "postgres".into();
    bootstrap.credentials.name = format!("{cluster}-superuser");
    let name = format!(
        "bootstrap-{}",
        wamn_digest::bytes_sha256(connection.database.as_bytes())[7..27].to_owned()
    );
    let input = bootstrap.to_json()?;
    let mut job = wamn_kubernetes::job::manifest(
        &name,
        image,
        &["/usr/local/bin/wamn-control"],
        &["bootstrap-project", "--connection", &input],
    );
    job["spec"]["ttlSecondsAfterFinished"] = 3600.into();
    job["spec"]["template"]["spec"]["automountServiceAccountToken"] = true.into();
    job["spec"]["template"]["spec"]["serviceAccountName"] = format!("{name}-account").into();
    let directory = tempfile::tempdir()?;
    wamn_kubernetes::job::run(target, directory.path(), &mut job, &mut []).await?;
    Ok(true)
}

/// The private bootstrap worker refuses an existing baseline before any DDL.
pub async fn initialize_database(input: &str) -> anyhow::Result<()> {
    use sqlx::Connection as _;
    let mut client = wamn_postgres::connect(input).await?;
    let mut tx = client.begin().await?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('wamn.initial-project-baseline',0))",
    )
    .execute(&mut *tx)
    .await?;
    let present: bool = sqlx::query_scalar("SELECT to_regclass('platform.\"user\"') IS NOT NULL")
        .fetch_one(&mut *tx)
        .await?;
    anyhow::ensure!(
        !present,
        "initial bootstrap refuses an existing project baseline"
    );
    crate::platform_migrations::install_project(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

//! Database provisioning inputs rendered into the project chart.

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_application_chart::{RoleName, release_name, workload_name};
use wamn_platform_install::{database::ConnectionConfig, provision_org::APPLY_SECRET};
use wamn_registry::Triple;

use crate::migrations::MigrationDatabase;

/// Set worker and credential values on the project release.
///
/// # Errors
/// Returns an error when the coordinates cannot name an environment resource.
pub fn set_operation_values<'a>(
    values: &mut Value,
    triple: &Triple,
    cluster: &str,
    database: &str,
    applications: impl Iterator<Item = &'a wamn_registry::ApplicationName>,
) -> anyhow::Result<()> {
    let name = crate::environment_names::environment_resource_name(
        &triple.org,
        &triple.project,
        &triple.env,
    )?;
    let capture = crate::cdc_reader_name(&triple.org, &triple.project, &triple.env);
    let diff_workloads: Vec<_> = applications
        .map(|application| {
            let release = release_name(&triple.org, &triple.project, &triple.env, application);
            workload_name(&release, RoleName::Http)
        })
        .collect();
    let diff_secrets = [
        APPLY_SECRET.into(),
        "wamn-pg-ca".into(),
        MigrationDatabase::Project.credential_secret(database),
        format!("{cluster}-ca"),
    ];
    values["operations"] = json!({
        "name":name,"cluster":cluster,
        "capture":capture,
        "hostGroup":wamn_registry::name::environment_host_name(&triple.org, &triple.env),
        "controlDeployment":wamn_registry::name::control_resource(&triple.org),
        "diffWorkloads":diff_workloads,
        "diffSecrets":diff_secrets,
    });
    values["credentials"] = json!({
        "name":wamn_registry::credential_secret_name(&triple.org, &triple.project, &triple.env),
        "controlAccount":wamn_registry::name::control_service_account(&triple.org),
        "platformLogin":wamn_registry::name::project_login_certificate(database, wamn_role::PLATFORM_LOGIN),
        "serverCa":wamn_registry::name::project_server_ca_copy(cluster),
    });
    Ok(())
}

pub fn publication_values(
    triple: &wamn_registry::Triple,
    instance: &wamn_registry::instance_suffix::InstanceSuffix,
    cluster: &str,
    name: &str,
    schemas: &[wamn_package::canonical_name::CanonicalName],
) -> Value {
    json!({"publication":{"name":name,"cluster":cluster,"org":triple.org,"project":triple.project,"env":triple.env,
        "database":crate::project_env_database_name(&triple.org,&triple.project,&triple.env,instance),
        "schemas":schemas,"publish":crate::sql::CDC_PUBLISH}})
}

/// Project sizing and role values for the chart.
pub fn database_values(
    connection: &ConnectionConfig,
    replicas: u32,
    mut sizing: Value,
    settings: &Value,
) -> anyhow::Result<Value> {
    use wamn_registry::name;
    let database = &connection.database;
    let cluster = name::project_cluster(database);
    let roles = crate::roles::project(database, replicas)?;
    let max_connections = roles.iter().map(|role| role.connection_limit).sum::<u32>() + 10;
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
    sizing["initializeRoles"] = crate::sql::ensure_project_roles_sql().into();
    let extensions = crate::sql::PLATFORM_EXTENSIONS.map(|name| json!({"name":name}));
    Ok(json!({
        "namespace":connection.credentials.namespace,
        "cluster":sizing,
        "database":{
            "name":database,"cluster":cluster,"owner":connection.username,"extensions":extensions
        }
    }))
}

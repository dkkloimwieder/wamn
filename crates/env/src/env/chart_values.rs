//! Project deployment chart values.

use crate::app_chart::plan::{release_name, workload_name};
use serde_json::{Value, json};
use wamn_platform_install::provision_org::APPLY_SECRET;
use wamn_registry::{Triple, name};

/// Set worker and credential values on the project release.
///
/// # Errors
/// Returns an error when the coordinates cannot name an environment resource.
pub fn set_operation_values<'a>(
    values: &mut Value,
    triple: &Triple,
    cluster: &str,
    database: &str,
    apps: impl Iterator<Item = &'a wamn_registry::AppName>,
) -> anyhow::Result<()> {
    let name = {
        wamn_platform_install::env::validate_project_env(
            &triple.org,
            &triple.project,
            &triple.env,
        )?;
        name::project_env_resource_name(&triple.org, &triple.project, &triple.env)
    };
    let capture = name::cdc_reader_name(&triple.org, &triple.project, &triple.env);
    let diff_workloads: Vec<_> = apps
        .map(|app| {
            let release = release_name(&triple.org, &triple.project, &triple.env, app);
            workload_name(&release)
        })
        .collect();
    let diff_secrets = [
        APPLY_SECRET.into(),
        "wamn-pg-ca".into(),
        name::project_login_certificate(database, wamn_role::MIGRATION_ROLE),
        format!("{cluster}-ca"),
    ];
    values["operations"] = json!({
        "name":name,"cluster":cluster,
        "capture":capture,
        "hostGroup":wamn_registry::name::env_host_name(&triple.org, &triple.env),
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
        "database":name::project_env_database_name(&triple.org,&triple.project,&triple.env,instance),
        "schemas":schemas,"publish":wamn_event::reader::CDC_PUBLISH}})
}

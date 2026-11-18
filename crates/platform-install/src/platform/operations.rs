//! Project chart values for schema diff, change capture, and worker configuration.
use serde_json::{Value, json};
use wamn_provision::migrations::MigrationDatabase;
use wamn_registry::Triple;

use super::installation::{SYSTEM_CLUSTER, SYSTEM_DATABASE};

pub fn values<'a>(
    triple: &Triple,
    cluster: &str,
    database: &str,
    applications: impl Iterator<Item = &'a wamn_catalog::ApplicationName>,
) -> anyhow::Result<Value> {
    let name = wamn_provision::environment_names::environment_resource_name(
        &triple.org,
        &triple.project,
        &triple.env,
    )?;
    let capture = wamn_provision::cdc_reader_name(
        &triple.org,
        &triple.project,
        &triple.env,
    );
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
        MigrationDatabase::System.managed_role(SYSTEM_DATABASE).password_secret.name,
        format!("{SYSTEM_CLUSTER}-ca"),
        MigrationDatabase::Project.managed_role(database).password_secret.name,
        format!("{cluster}-ca"),
    ];
    Ok(json!({
        "operations":{
            "name":name,"cluster":cluster,
            "capture":capture,"captureSecret":capture_secret,
            "hostGroup":wamn_registry::name::environment_host_name(&triple.org, &triple.env),
            "controlDeployment":wamn_registry::name::control_resource(&triple.org),
            "diffWorkloads":diff_workloads,
            "diffSecrets":diff_secrets,
        },
        "credentials":{
            "name":wamn_registry::credential_secret_name(&triple.org, &triple.project, &triple.env),
            "controlAccount":wamn_registry::name::control_service_account(&triple.org),
        }
    }))
}

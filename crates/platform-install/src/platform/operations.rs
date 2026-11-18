//! Environment worker accounts are owned by the project chart.
use serde_json::{Value, json};
use wamn_registry::Triple;

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
    let capture = wamn_provision::project_env_cdc_secret_name(
        &triple.org,
        &triple.project,
        &triple.env,
    );
    let application_releases: Vec<_> = applications
        .map(|application| {
            wamn_catalog::resource_names::application_release_name(
                &triple.org,
                &triple.project,
                &triple.env,
                application,
            )
        })
        .collect();
    Ok(json!({
        "operations":{
            "name":name,"cluster":cluster,"database":database,"capture":capture,
            "hostGroup":wamn_registry::name::environment_host_name(&triple.org, &triple.env),
            "controlDeployment":wamn_registry::name::control_resource(&triple.org),
            "applicationReleases":application_releases,
        },
        "credentials":{
            "name":wamn_registry::credential_secret_name(&triple.org, &triple.project, &triple.env),
            "controlAccount":wamn_registry::name::control_service_account(&triple.org),
        }
    }))
}

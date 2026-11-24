#[doc(inline)]
pub use wamn_kubernetes::release_chart::bundle;

/// Validate the environment coordinates and derive the application release name.
pub fn release_name(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    application: &wamn_registry::ApplicationName,
) -> anyhow::Result<String> {
    wamn_provision::validate_project_env(org, project, env)?;
    Ok(wamn_application_chart::release_name(
        org,
        project,
        env,
        application,
    ))
}

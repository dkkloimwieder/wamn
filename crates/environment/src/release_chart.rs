/// Validate the environment coordinates and derive the application release name.
pub fn release_name(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    application: &wamn_registry::ApplicationName,
) -> anyhow::Result<String> {
    wamn_platform_install::environment::validate_project_env(org, project, env)?;
    Ok(crate::application_chart::plan::release_name(
        org,
        project,
        env,
        application,
    ))
}

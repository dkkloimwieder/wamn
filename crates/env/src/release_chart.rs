/// Validate the environment coordinates and derive the application release name.
pub fn release_name(
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    app: &wamn_registry::AppName,
) -> anyhow::Result<String> {
    wamn_platform_install::env::validate_project_env(org, project, env)?;
    Ok(crate::app_chart::plan::release_name(org, project, env, app))
}

use super::{
    Triple, WorkloadRoleFamily, WorkloadRoleIdentity, WorkloadRoleIdentityType, cdc_object_name,
    project_env_database_name, workload_login,
};
use anyhow::Context as _;

/// The families whose logins name the instance's database: a
/// control or org family's target names the control database, so its roles
/// stay with the triple or the org.
pub fn instance_families() -> impl Iterator<Item = WorkloadRoleFamily> {
    WorkloadRoleFamily::ALL.into_iter().filter(|family| {
        matches!(
            family.identity_type(),
            WorkloadRoleIdentityType::ProjectEnvKey | WorkloadRoleIdentityType::ProjectEnvironment
        )
    })
}

/// The roles of one instance: capture and one login per workload family.
///
/// # Errors
///
/// When a role name cannot be derived.
pub fn instance_roles(
    triple: &Triple,
    instance: &str,
    project_env_key: &str,
) -> anyhow::Result<Vec<String>> {
    let (org, project, env) = (
        triple.org.as_str(),
        triple.project.as_str(),
        triple.env.as_str(),
    );
    let database = project_env_database_name(org, project, env, instance);
    let mut roles = vec![cdc_object_name(org, project, env, instance)];
    for family in instance_families() {
        let target = match family.identity_type() {
            WorkloadRoleIdentityType::ProjectEnvKey => WorkloadRoleIdentity::ProjectEnvKey {
                project_env_key,
                database: &database,
            },
            _ => WorkloadRoleIdentity::ProjectEnvironment {
                org,
                project,
                environment: env,
                database: &database,
            },
        };
        roles.push(
            workload_login(family, target)
                .with_context(|| format!("derive the {family:?} login"))?,
        );
    }
    Ok(roles)
}

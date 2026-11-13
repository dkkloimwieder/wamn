use super::{
    CredentialGeneration, Triple, WorkloadRoleFamily, WorkloadRoleScope, WorkloadRoleScopeKind,
    cdc_object_name, project_env_database_name, workload_generation_role,
};
use anyhow::Context as _;

/// The families whose generation roles name the instance's database: a
/// control or org family's scope names the control database, so its roles
/// stay with the triple or the org.
pub fn instance_families() -> impl Iterator<Item = WorkloadRoleFamily> {
    WorkloadRoleFamily::ALL.into_iter().filter(|family| {
        matches!(
            family.scope_kind(),
            WorkloadRoleScopeKind::EnvKey | WorkloadRoleScopeKind::ProjectEnvironment
        )
    })
}

/// The roles of one instance: the CDC role, then `a` and `b` of each
/// instance family.
///
/// # Errors
///
/// When a role name cannot be derived.
pub fn instance_roles(
    triple: &Triple,
    instance: &str,
    env_key: &str,
) -> anyhow::Result<Vec<String>> {
    let (org, project, env) = (
        triple.org.as_str(),
        triple.project.as_str(),
        triple.env.as_str(),
    );
    let database = project_env_database_name(org, project, env, instance);
    let mut roles = vec![cdc_object_name(org, project, env, instance)];
    for family in instance_families() {
        let scope = match family.scope_kind() {
            WorkloadRoleScopeKind::EnvKey => WorkloadRoleScope::EnvKey {
                env_key,
                database: &database,
            },
            _ => WorkloadRoleScope::ProjectEnvironment {
                org,
                project,
                environment: env,
                database: &database,
            },
        };
        for generation in [CredentialGeneration::A, CredentialGeneration::B] {
            roles.push(
                workload_generation_role(family, scope, generation)
                    .with_context(|| format!("derive the {family:?} generation role"))?,
            );
        }
    }
    Ok(roles)
}

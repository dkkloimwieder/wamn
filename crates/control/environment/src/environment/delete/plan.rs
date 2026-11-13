use super::{
    CredentialGeneration, ErrorCode, GetStreamErrorKind, Triple, WorkloadRoleFamily,
    WorkloadRoleScope, WorkloadRoleScopeKind, cdc_object_name, escape_identifier, escape_literal,
    project_env_database_name, workload_generation_role,
};
use anyhow::Context as _;

/// The statements that delete one tenant's rows of one control table. An
/// immutable table gets its trigger off for the one delete only.
pub fn delete_control_rows_sql(table: &str, trigger: Option<&str>, env_key: &str) -> Vec<String> {
    let delete = format!(
        "DELETE FROM {table} WHERE env_key = {}",
        escape_literal(env_key)
    );
    match trigger {
        Some(trigger) => vec![
            format!(
                "ALTER TABLE {table} DISABLE TRIGGER {}",
                escape_identifier(trigger)
            ),
            delete,
            format!(
                "ALTER TABLE {table} ENABLE TRIGGER {}",
                escape_identifier(trigger)
            ),
        ],
        None => vec![delete],
    }
}

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

/// The credential files to destroy before the commit, from the removed
/// credentials' `ref, version, target_type`. A cloud target's credentials are
/// in the Secret that the delete removes, whatever store a row names.
pub(super) fn credential_files(
    credentials: impl IntoIterator<Item = (String, String, String)>,
) -> Vec<(String, String)> {
    credentials
        .into_iter()
        .filter(|(_, _, target_type)| target_type == "local")
        .map(|(reference, version, _)| (reference, version))
        .collect()
}

pub(super) fn stream_not_found(kind: GetStreamErrorKind) -> bool {
    matches!(kind, GetStreamErrorKind::JetStream(error) if error.error_code() == ErrorCode::STREAM_NOT_FOUND)
}

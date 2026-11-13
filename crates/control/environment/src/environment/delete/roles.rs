//! Environment-targeted logins on the shared control database.

use anyhow::Context as _;
use postgres_protocol::escape::escape_identifier;
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleIdentity, WorkloadRoleIdentityType,
    workload_generation_role,
};
use wamn_package_control::database::connect;
use wamn_registry::Triple;

pub(super) async fn delete_control_roles(
    admin_url: &str,
    triple: &Triple,
) -> anyhow::Result<Vec<String>> {
    let mut admin = connect(admin_url).await?;
    let database: String = admin
        .query_one("SELECT current_database()", &[])
        .await?
        .get(0);
    let target = WorkloadRoleIdentity::Control {
        org: &triple.org,
        project: &triple.project,
        environment: &triple.env,
        database: &database,
    };
    let mut roles = Vec::new();
    for family in WorkloadRoleFamily::project_env_families()
        .filter(|family| family.identity_type() == WorkloadRoleIdentityType::Control)
    {
        for generation in [CredentialGeneration::A, CredentialGeneration::B] {
            let role = workload_generation_role(family, target, generation)?;
            if admin
                .query_opt("SELECT 1 FROM pg_roles WHERE rolname=$1", &[&role])
                .await?
                .is_some()
            {
                let transaction = admin.transaction().await?;
                transaction
                    .batch_execute(
                        &wamn_control_provision::sql::retire_workload_generation_sql(
                            family, &database, &role,
                        ),
                    )
                    .await?;
                transaction
                    .batch_execute(&format!("DROP ROLE {}", escape_identifier(&role)))
                    .await
                    .with_context(|| format!("drop environment control role {role}"))?;
                transaction.commit().await?;
            }
            roles.push(role);
        }
    }
    Ok(roles)
}

//! CloudNativePG managed roles for project databases.

use wamn_provision::WorkloadRoleFamily;
use wamn_provision::connection::{ManagedRole, managed_replication_role, managed_role};
use wamn_registry::name::project_login_secret;

/// Derive project role limits from the environment host replica count.
pub(crate) fn project(database: &str, replicas: u32) -> anyhow::Result<Vec<ManagedRole>> {
    // Keep max_connections within PostgreSQL's supported range.
    anyhow::ensure!(
        replicas <= (262_143 - 22) / 40,
        "host replica count exceeds the PostgreSQL connection budget"
    );
    let mut roles = Vec::new();
    for family in WorkloadRoleFamily::ALL {
        let name = family.login_name();
        let mut role = managed_role(
            name.parse()?,
            family.acl_role(),
            project_login_secret(database, name),
        );
        role.connection_limit = Some(match family {
            WorkloadRoleFamily::App | WorkloadRoleFamily::Query => replicas * 16,
            WorkloadRoleFamily::Platform => replicas * 8 + 4,
            WorkloadRoleFamily::Migration => 4,
        });
        roles.push(role);
    }
    let cdc = wamn_role::CDC_LOGIN;
    let mut role = managed_replication_role(cdc.parse()?, project_login_secret(database, cdc));
    role.connection_limit = Some(4);
    roles.push(role);
    Ok(roles)
}

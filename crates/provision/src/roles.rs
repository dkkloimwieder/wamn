//! CloudNativePG managed roles for project databases.

use serde::Serialize;
use wamn_identity_protocol::workload_role::WorkloadRoleFamily;
use wamn_registry::name::project_login_certificate;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectRole {
    name: &'static str,
    certificate_secret: String,
    pub(crate) connection_limit: u32,
    in_roles: [&'static str; 1],
    inherit: bool,
    replication: bool,
}

/// Derive project role limits from the environment host replica count.
pub(crate) fn project(database: &str, replicas: u32) -> anyhow::Result<Vec<ProjectRole>> {
    // Keep max_connections within PostgreSQL's supported range.
    anyhow::ensure!(
        replicas <= (262_143 - 22) / 40,
        "host replica count exceeds the PostgreSQL connection budget"
    );
    let mut roles = Vec::new();
    for family in WorkloadRoleFamily::ALL {
        let name = family.login_name();
        let connection_limit = match family {
            WorkloadRoleFamily::App | WorkloadRoleFamily::Query => replicas * 16,
            WorkloadRoleFamily::Platform => replicas * 8 + 4,
            WorkloadRoleFamily::Migration => 4,
        };
        roles.push(ProjectRole {
            name,
            certificate_secret: project_login_certificate(database, name),
            connection_limit,
            in_roles: [family.acl_role()],
            inherit: true,
            replication: false,
        });
    }
    let cdc = wamn_role::CDC_LOGIN;
    roles.push(ProjectRole {
        name: cdc,
        certificate_secret: project_login_certificate(database, cdc),
        connection_limit: 4,
        in_roles: [wamn_role::CHECKPOINT_ROLE],
        inherit: false,
        replication: true,
    });
    Ok(roles)
}

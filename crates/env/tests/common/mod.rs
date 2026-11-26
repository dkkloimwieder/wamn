//! Logins that CNPG manages, created the way it creates them.

use postgres_protocol::escape::{escape_identifier, escape_literal};
use wamn_env::project_setup::{ensure_workload_acl_role_sql, grant_workload_login_sql};
use wamn_identity_protocol::workload_role::WorkloadRoleFamily;

/// Create a password login of `family` as CNPG would, with the grants that provisioning adds.
pub fn login_sql(
    family: WorkloadRoleFamily,
    database: &str,
    role: &wamn_role::LoginName,
    password: &str,
) -> String {
    let login = escape_identifier(role.as_str());
    format!(
        "{} CREATE ROLE {login} LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD {}; \
         GRANT {} TO {login} WITH ADMIN FALSE, INHERIT TRUE, SET FALSE; {}",
        ensure_workload_acl_role_sql(family),
        escape_literal(password),
        escape_identifier(family.acl_role()),
        grant_workload_login_sql(family, database, role)
    )
}

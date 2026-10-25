//! Package object owners and their platform history privileges.

use super::{ensure_acl_role_sql, quote_ident, quote_literal};
use crate::package_owner_role;

/// Provision the package owner before creating its schemas and objects.
pub fn ensure_package_owner_role_sql(package: &str) -> String {
    let role = package_owner_role(package);
    format!(
        "{} DO $package_owner$ BEGIN \
           IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_provisioner') \
             AND NOT EXISTS (SELECT FROM pg_catalog.pg_auth_members m \
               JOIN pg_catalog.pg_roles owner_role ON owner_role.oid = m.roleid \
               JOIN pg_catalog.pg_roles provisioner ON provisioner.oid = m.member \
               WHERE owner_role.rolname = {role_literal} AND provisioner.rolname = 'wamn_provisioner' \
               GROUP BY m.roleid, m.member \
               HAVING bool_or(m.admin_option) AND bool_or(m.inherit_option) AND bool_or(m.set_option)) THEN \
             GRANT {role} TO wamn_provisioner WITH ADMIN TRUE, INHERIT TRUE, SET TRUE; \
           END IF; END $package_owner$;",
        ensure_acl_role_sql(&role),
        role = quote_ident(&role),
        role_literal = quote_literal(&role),
    )
}

/// Preserve the history surface formerly used through the database owner.
/// These invoker routines confer no access to platform tables.
pub fn package_owner_grants_sql(package: &str) -> String {
    let role = quote_ident(&package_owner_role(package));
    format!(
        "GRANT USAGE, CREATE ON SCHEMA wamn_history TO {role}; \
         GRANT EXECUTE ON FUNCTION \
           wamn_history.stamp_row(), \
           wamn_history.create_history_table(text, text, boolean), \
           wamn_history.row_image(record), \
           wamn_history.timestamptz_image(timestamptz), \
           wamn_history.log_row_change() TO {role};"
    )
}

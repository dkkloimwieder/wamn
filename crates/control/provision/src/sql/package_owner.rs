//! Package object owners and their platform history privileges.

use super::{ensure_acl_role_sql, escape_identifier};
use crate::package_owner_role;

/// Provision the package owner before creating its schemas and objects.
pub fn ensure_package_owner_role_sql(package: &str) -> String {
    let role = package_owner_role(package);
    ensure_acl_role_sql(&role)
}

/// Preserve the history surface formerly used through the database owner.
/// These invoker routines confer no access to platform tables.
pub fn package_owner_grants_sql(package: &str) -> String {
    let role = escape_identifier(&package_owner_role(package));
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

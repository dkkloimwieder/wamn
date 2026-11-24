//! Statements for system database bootstrap.

/// Grant the functions required to initialize control row policies for a reader.
/// The caller supplies a quoted role identifier.
pub fn grant_control_policy_execute_sql(role: &str) -> String {
    format!(
        " GRANT EXECUTE ON FUNCTION registry.login_org(), registry.login_principal(uuid) TO {role};",
    )
}

/// Set the installation's domain and project stream limit.
pub const UPDATE_PLATFORM_META_SQL: &str = "UPDATE registry.meta SET platform_domain = $1, max_projects = $2 \
     WHERE (platform_domain, max_projects) IS DISTINCT FROM ($1, $2) \
     RETURNING old.max_projects IS NULL";

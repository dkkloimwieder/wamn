//! Statements for system database bootstrap.

/// Grant the functions required to initialize control row policies for a reader.
/// The caller supplies a quoted role identifier.
pub fn grant_control_policy_execute_sql(role: &str) -> String {
    format!(
        " GRANT EXECUTE ON FUNCTION registry.login_org(), registry.login_principal(uuid) TO {role};",
    )
}

/// Record the installation's platform domain if it changed.
pub const UPDATE_PLATFORM_META_SQL: &str = "UPDATE registry.meta SET platform_domain = $1 \
     WHERE platform_domain IS DISTINCT FROM $1";

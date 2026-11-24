//! Shared statements for the system database bootstrap and project reservations.

/// Grant the functions required to initialize control row policies for a reader.
/// The caller supplies a quoted role identifier.
pub fn grant_control_policy_execute_sql(role: &str) -> String {
    format!(
        " GRANT EXECUTE ON FUNCTION registry.login_org(), registry.login_principal(uuid) TO {role};",
    )
}

/// Serialize writes to the project stream limit and reservations against it.
pub const PROJECT_STREAM_CAPACITY_LOCK_SQL: &str =
    "SELECT pg_advisory_xact_lock(hashtextextended('wamn.project-stream-capacity', 0))";

/// Set the installation's domain and project stream limit.
pub const UPDATE_PLATFORM_META_SQL: &str = "UPDATE registry.meta SET platform_domain = $1, max_projects = $2 \
     WHERE (platform_domain, max_projects) IS DISTINCT FROM ($1, $2) \
     RETURNING old.max_projects IS NULL";

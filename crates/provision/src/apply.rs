//! The environment apply login and its system database grants.

/// The fixed login used outside the privileged provisioning phase.
pub const APPLY_ROLE: &str = "wamn_apply";

/// Create the apply login without system-owner membership.
pub fn ensure_apply_role_sql() -> &'static str {
    "GRANT wamn_db_owner TO wamn_apply WITH ADMIN FALSE, INHERIT TRUE, SET TRUE;"
}

/// Exact privileges used by the apply phases on the system database.
pub const APPLY_GRANTS_SQL: &str = include_str!("../../../deploy/sql/apply-grants.sql");

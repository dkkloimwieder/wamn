//! The environment apply login and its system database grants.

/// Exact privileges used by the apply phases on the system database.
pub const APPLY_GRANTS_SQL: &str = include_str!("../../../deploy/sql/apply-grants.sql");

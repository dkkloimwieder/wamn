//! Pure Postgres text builders for project provisioning (SR3 house rule 3:
//! pure text + validated/quoted identifiers; the driver holds the connection).
//!
//! Every builder takes an **already-validated** name (see
//! [`crate::name::validate_project_env`]); a database name it takes is
//! double-quoted, so a slug (which cannot contain a `"`) is injection-safe. The
//! `wamn_app` role name is a pinned constant. Values that vary (a probed role
//! name, a role password) travel as `$n` params or quoted literals.

mod database;

pub use database::{
    PLATFORM_EXTENSIONS, create_database_named_sql, drop_database_named_sql,
    install_platform_extensions_sql, set_database_owner_sql,
};

use crate::project_setup::ensure_project_roles_sql;

/// Create the shared project privilege roles before installing project schemas.
pub fn ensure_app_acl_role_sql() -> String {
    ensure_project_roles_sql().to_owned()
}

/// The `catalog.workflow` columns that callable-HTTP connection authorization
/// reads: the key of one workflow version and its graph. The admitter gets
/// these columns only, not the whole table.
pub const HTTP_ADMITTER_WORKFLOW_COLUMNS: [&str; 5] =
    ["env", "app", "workflow_id", "version", "graph_json"];

/// The schemas that the callable-HTTP admitter uses.
pub const HTTP_ADMITTER_SCHEMAS: [&str; 2] = ["catalog", "platform"];

/// Catalog relations the executor-platform family reads (`wamn-0h0g.22.37`).
///
/// Released and candidate resolution read these through the platform pool,
/// and the host's workflow admission and resolution read `workflow`.
///
/// TABLE grain, not column grain — the [`grant_system_reader_surface_sql`]
/// determination: this family holds no write privilege anywhere in `catalog`,
/// so a column list would withhold only labels and timestamps while making a
/// statement that reads one more column fail in production instead of at review.
pub const EXECUTOR_PLATFORM_CATALOG_RELATIONS: [&str; 3] =
    ["workflow", "connection_requirement", "releases"];

/// Columns written by workflow checkpoint and terminal transitions.
pub const EXECUTOR_PLATFORM_RUN_UPDATE_COLUMNS: [&str; 6] = [
    "status",
    "terminal_reason",
    "fail_type",
    "result_json",
    "state_json",
    "updated_at",
];

/// Columns written when a host renews or replaces a lease.
pub const EXECUTOR_PLATFORM_QUEUE_UPDATE_COLUMNS: [&str; 4] = [
    "lease_owner",
    "lease_expires_at",
    "lease_generation",
    "attempts",
];

/// Run columns written during admission.
pub const EXECUTOR_PLATFORM_RUN_INSERT_COLUMNS: [&str; 16] = [
    "package_id",
    "env",
    "graph_id",
    "graph_version",
    "graph_hash",
    "trigger_source",
    "execution_principal_id",
    "execution_role",
    "invoked_by",
    "invoked_by_user_id",
    "idempotency_key",
    "input_json",
    "status",
    "event_source_run_id",
    "event_root_run_id",
    "event_depth",
];

/// Queue columns inserted with the first lease.
pub const EXECUTOR_PLATFORM_QUEUE_INSERT_COLUMNS: [&str; 4] = [
    "run_id",
    "lease_owner",
    "lease_expires_at",
    "lease_generation",
];

/// Create the shared project privilege roles, including the platform group.
pub fn ensure_platform_group_role_sql() -> String {
    ensure_project_roles_sql().to_owned()
}

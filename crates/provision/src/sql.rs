//! Pure Postgres text builders for project provisioning (SR3 house rule 3:
//! pure text + validated/quoted identifiers; the driver holds the connection).
//!
//! Every builder takes an **already-validated** name (see
//! [`crate::validate_project_env`]); a database name it takes is
//! double-quoted, so a slug (which cannot contain a `"`) is injection-safe. The
//! `wamn_app` role name is a pinned constant. Values that vary (a probed role
//! name, a role password) travel as `$n` params or quoted literals.

mod cdc;
mod credentials;
mod database;
mod database_grants;

pub use database::{
    PLATFORM_EXTENSIONS, create_database_named_sql, drop_database_named_sql,
    install_platform_extensions_sql, set_database_owner_sql,
};

pub use credentials::{
    ensure_workload_acl_role_sql, grant_dynamic_sql_surface_sql, grant_platform_surface_sql,
    grant_workload_login_sql,
};

#[doc(inline)]
pub use database_grants::{
    non_template_databases_sql, public_connect_databases_sql,
    public_temporary_on_current_database_sql, revoke_public_connect_base_grants_sql,
    role_database_grants_sql,
};

#[doc(inline)]
pub use cdc::{
    CDC_PUBLISH, PUBLICATION_PROBE_SQL, create_failover_slot_sql, drop_replication_slot_sql,
    ensure_cdc_exclusion_map_sql, ensure_entity_map_sql, grant_replication_access_sql,
    upsert_cdc_exclusion_map_sql, upsert_entity_map_sql,
};

use crate::workload_role::WorkloadRoleFamily;
pub(crate) use postgres_protocol::escape::escape_identifier;
use postgres_protocol::escape::escape_literal;

/// Create shared project roles before selecting the database owner.
pub fn ensure_project_roles_sql() -> &'static str {
    include_str!("../../../deploy/sql/project-roles.sql")
}

/// Create shared system roles before selecting the database owner.
pub fn ensure_system_roles_sql() -> &'static str {
    include_str!("../../../deploy/sql/system-roles.sql")
}

/// Create the shared project privilege roles before installing project schemas.
pub fn ensure_app_acl_role_sql() -> String {
    ensure_project_roles_sql().to_owned()
}

/// Catalog relations read by callable-HTTP connection authorization.
///
pub const HTTP_ADMITTER_CATALOG_RELATIONS: [&str; 2] = ["releases", "connection_requirement"];

/// The `catalog.workflows` columns that callable-HTTP connection authorization
/// reads: the key of one workflow version and its graph. The admitter gets
/// these columns only, not the whole table.
pub const HTTP_ADMITTER_WORKFLOW_COLUMNS: [&str; 5] = [
    "environment",
    "application",
    "workflow_id",
    "version",
    "graph_json",
];

/// The schemas that the callable-HTTP admitter uses.
pub const HTTP_ADMITTER_SCHEMAS: [&str; 2] = ["catalog", "platform"];

/// Every relation that the callable-HTTP admitter reads, as `(schema, relation)`.
///
/// [`grant_http_admitter_surface_sql`] grants `SELECT` on each, and the setup
/// test expects exactly these grants, so the two cannot drift.
pub fn http_admitter_relations() -> impl Iterator<Item = (&'static str, &'static str)> {
    HTTP_ADMITTER_CATALOG_RELATIONS
        .iter()
        .map(|relation| ("catalog", *relation))
        .chain([
            ("platform", "connection"),
            ("platform", "permission"),
            ("platform", "setting"),
            ("platform", "user"),
            ("platform", "user_role"),
        ])
}

/// Catalog relations the executor-platform family reads (`wamn-0h0g.22.37`).
///
/// Released and candidate resolution read these through the platform pool,
/// and the host's workflow admission and resolution read `workflows`.
///
/// TABLE grain, not column grain — the [`grant_system_reader_surface_sql`]
/// determination: this family holds no write privilege anywhere in `catalog`,
/// so a column list would withhold only labels and timestamps while making a
/// statement that reads one more column fail in production instead of at review.
pub const EXECUTOR_PLATFORM_CATALOG_RELATIONS: [&str; 3] =
    ["workflows", "connection_requirement", "releases"];

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
pub const EXECUTOR_PLATFORM_QUEUE_UPDATE_COLUMNS: [&str; 4] =
    ["lease_owner", "lease_expires_at", "lease_generation", "attempts"];

/// Run columns written during admission.
pub const EXECUTOR_PLATFORM_RUN_INSERT_COLUMNS: [&str; 16] = [
    "package_id",
    "environment",
    "wiring_id",
    "wiring_version",
    "wiring_hash",
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

/// The application authorization relations the administration family reads and
/// writes (docs/architecture/data-access.md#actors), the environment status
/// that the status routes mirror
/// (docs/architecture/execution.md#host-run-routes), and the platform
/// configuration tables that the control host writes
/// (docs/architecture/data-access.md#actors). `number_counter` is not one of
/// them: only `platform.bump_counter`, `platform.assign_counter` and
/// `platform.set_counter` write it.
pub const ADMINISTRATION_RELATIONS: [&str; 14] = [
    "user",
    "role",
    "user_role",
    "permission",
    "environment",
    "external_system",
    "connection",
    "file",
    "connection_generation",
    "credential",
    "oauth_token",
    "oauth_authorization",
    "number_sequence",
    "setting",
];

// --- T1 control-database read surfaces (wamn-0h0g.12.116, wamn-0h0g.12.67) ---
//
// The T1 system database's two read-only consumers both authenticated as
// `wamn_system`, the AUTHORIZATION owner of all three schemas below.
// `deploy/sql/system-schema.sql` declares the `login_org` policies, but they
// confine only members of `wamn_control` or `wamn_apply`, and it forces no
// row-level security, so the owner bypasses them. For these consumers,
// confinement in this database is PURELY GRANT-SHAPED: there is no
// project predicate to satisfy and none is invented here. The owner credential is
// therefore not merely wide, it is unconfined — it can INSERT an identity credential
// row binding any token digest to any principal, or an `identity.project_roles`
// row self-granting a project role anywhere.
//
// TWO families and TWO builders, never one. The grant sets are disjoint and the
// disjointness IS the security property: one role serving both is how a
// credential gets widened to the union. Each builder revokes across ALL of
// [`SYSTEM_PLANE_SCHEMAS`] before granting inside its own, so a role widened out
// of band converges back to its own schema on the next apply rather than merely
// gaining whatever surface it was missing. A table-level `REVOKE` also clears
// that table's column-level ACL entries, so no per-column revocation loop is
// needed to make the narrowing total.

/// Every schema `deploy/sql/system-schema.sql` creates under `wamn_system`.
///
/// The REVOKE target, not the grant target: a system-plane reader is revoked
/// across all three and granted inside exactly one.
pub const SYSTEM_PLANE_SCHEMAS: [&str; 3] = ["identity", "provisioning", "registry"];

/// System identity relations read by the environment reader.
pub const IDENTITY_READER_RELATIONS: [&str; 4] = [
    "principals",
    "project_env_memberships",
    "project_roles",
    "password_logins",
];

/// Session admission tests the active org membership (`wamn-ms1sn.2`).
pub const IDENTITY_READER_COLUMNS: [(&str, &[&str]); 1] =
    [("org_memberships", &["principal_id", "org", "status"])];

/// Non-secret connection functions available to the control host.
pub const CONTROL_CONNECTION_FUNCTIONS: &[&str] = &["configure_integration_type(text,text,jsonb)"];

/// The privileges the control host holds on a relation that it writes.
const CONTROL_WRITE: &[&str] = &["SELECT", "INSERT", "UPDATE", "DELETE"];

/// The control database relations an org's control host reaches, with the
/// privileges it holds on each (docs/architecture/execution.md#host-run-routes,
/// wamn-a40n.3). It reads who is a user and whether the password login of a
/// control session is live. It writes the org memberships and the org and project
/// grants of the org operations. It reads the projects and environments those grants cover. The `project.create` route writes a
/// project. It also stores encrypted OAuth tokens and pending authorizations.
pub const CONTROL_SURFACE: [(&str, &str, &[&str]); 18] = [
    ("identity", "org_memberships", CONTROL_WRITE),
    ("identity", "org_roles", CONTROL_WRITE),
    ("identity", "password_logins", &["SELECT"]),
    ("identity", "principals", &["SELECT", "INSERT", "UPDATE"]),
    ("identity", "project_env_memberships", CONTROL_WRITE),
    ("identity", "project_roles", CONTROL_WRITE),
    ("registry", "environment_projects", &["SELECT"]),
    ("registry", "environment_applications", &["SELECT"]),
    (
        "registry",
        "environment_application_connections",
        &["SELECT"],
    ),
    ("registry", "environment_integrations", &["SELECT"]),
    ("registry", "meta", &["SELECT"]),
    ("registry", "integration_type", &["SELECT"]),
    ("registry", "integration_type_version", &["SELECT"]),
    ("registry", "integration_type_auth", &["SELECT"]),
    ("registry", "auth_type", &["SELECT"]),
    ("registry", "target", &["SELECT"]),
    ("registry", "environments", &["SELECT"]),
    ("registry", "projects", &["SELECT", "INSERT"]),
];

/// The columns the control host writes in a relation of which it otherwise only
/// reads, with the privilege it holds on them. The status routes write the
/// status of an environment (docs/architecture/execution.md#host-run-routes,
/// wamn-zua8.2). The other columns of `registry.environment_projects` belong to
/// provisioning.
pub const CONTROL_COLUMN_SURFACE: [(&str, &str, &str, &[&str]); 1] =
    [("registry", "environments", "UPDATE", &["status"])];

/// The schemas of [`CONTROL_SURFACE`], in which the control role holds `USAGE`.
pub const CONTROL_SCHEMAS: [&str; 2] = ["identity", "registry"];

/// The family-specific privilege batch that converges one family's STABLE ACL
/// role to its exact grant set, when this crate owns that convergence.
///
/// `None` = the family's stable role carries no grant set this batch applies.
/// An admitted family lands on `None` with no edit here; a family
/// that acquires a grant set adds it beside its builder, which is the one place
/// a family stays per-family (`wamn-0h0g.22.16`).
pub fn stable_surface_sql(family: WorkloadRoleFamily) -> Option<String> {
    match family {
        WorkloadRoleFamily::Query => Some(grant_dynamic_sql_surface_sql()),
        WorkloadRoleFamily::Platform => Some(grant_platform_surface_sql()),
        _ => None,
    }
}

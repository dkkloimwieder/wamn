//! SQL for platform database roles and grants.

mod database_grants;

#[doc(inline)]
pub use database_grants::{
    non_template_databases_sql, public_connect_databases_sql,
    public_temporary_on_current_database_sql, revoke_public_connect_base_grants_sql,
    role_database_grants_sql,
};

/// Create shared system roles before selecting the database owner.
pub fn ensure_system_roles_sql() -> &'static str {
    include_str!("../../../deploy/sql/system-roles.sql")
}

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
    [("org_memberships", &["principal_id", "org", "active"])];

/// Non-secret connection functions available to the control host.
pub const CONTROL_CONNECTION_FUNCTIONS: &[&str] = &["configure_integration_type(text,text,jsonb)"];

/// The privileges the control host holds on a relation that it writes.
const CONTROL_WRITE: &[&str] = &["SELECT", "INSERT", "UPDATE", "DELETE"];

/// The control database relations an org's control host reaches, with the
/// privileges it holds on each (docs/architecture/execution.md#host-run-routes,
/// wamn-a40n.3). It reads who is a user and whether the password login of a
/// control session is live. It writes the org memberships and the org and project
/// grants of the org operations. It reads the projects and environments those grants cover. The `project.create` route writes a
/// project. It also stores encrypted OAuth tokens and pending authorizations and appends platform events.
pub const CONTROL_SURFACE: [(&str, &str, &[&str]); 19] = [
    ("identity", "org_memberships", CONTROL_WRITE),
    ("identity", "org_roles", CONTROL_WRITE),
    ("identity", "password_logins", &["SELECT"]),
    ("identity", "principals", &["SELECT", "INSERT", "UPDATE"]),
    ("identity", "project_env_memberships", CONTROL_WRITE),
    ("identity", "project_roles", CONTROL_WRITE),
    ("provisioning", "platform_event", &["INSERT"]),
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

/// The columns on which the control host holds an additional privilege.
pub const CONTROL_COLUMN_SURFACE: [(&str, &str, &str, &[&str]); 2] = [
    ("registry", "environments", "UPDATE", &["active"]),
    (
        "provisioning",
        "platform_event",
        "SELECT",
        &["command_id", "outcome"],
    ),
];

/// The schemas of [`CONTROL_SURFACE`], in which the control role holds `USAGE`.
pub const CONTROL_SCHEMAS: [&str; 3] = ["identity", "provisioning", "registry"];

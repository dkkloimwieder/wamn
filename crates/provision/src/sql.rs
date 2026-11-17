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
    ensure_db_owner_role_sql, install_platform_extensions_sql, set_database_owner_sql,
};

pub(crate) use credentials::ensure_acl_role_sql;
pub use credentials::{
    ensure_workload_acl_role_sql, grant_dynamic_sql_surface_sql, grant_workload_login_sql,
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

use crate::name::{APP_ROLE, DB_OWNER_ROLE};
use crate::workload_role::{
    PLATFORM_GROUP_ROLE, WorkloadRoleFamily,
};
pub(crate) use postgres_protocol::escape::escape_identifier;
use postgres_protocol::escape::escape_literal;

/// Create the two SQL privilege roles before installing project schemas.
///
/// Guest sessions authenticate as targeted [`WorkloadRoleFamily::App`]
/// generations and inherit this role's grants. The stable role therefore
/// carries no verifier and cannot authenticate or inherit memberships itself.
/// The shared bootstrap lock serializes cooperative appliers; the exception arm
/// hardens the role created by a concurrent non-cooperative applier. Callers
/// must commit this statement before applying
/// [`drain_app_role_sessions_sql`]. That boundary prevents a new shared-role
/// login from racing the drain while `NOLOGIN` is still uncommitted.
pub fn ensure_app_acl_role_sql() -> String {
    format!(
        "{} {}",
        ensure_acl_role_sql(APP_ROLE),
        ensure_acl_role_sql("wamn_query")
    )
}

/// Catalog relations read by callable-HTTP connection authorization.
///
pub const HTTP_ADMITTER_CATALOG_RELATIONS: [&str; 4] = [
    "releases",
    "connection_requirement",
    "connection_binding",
    "release_connection",
];

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
pub const EXECUTOR_PLATFORM_CATALOG_RELATIONS: [&str; 5] = [
    "workflows",
    "connection_requirement",
    "connection_binding",
    "release_connection",
    "releases",
];

/// Every `runs` column the executor-platform family WRITES, and no other
/// (`wamn-0h0g.22.37`).
///
/// The statements below update `runs` under
/// [`crate::workload_role::WorkloadRoleFamily::ExecutorPlatform`]:
///
/// * `queue::sql::clear_pre_effect_state_sql` — `state_json`
/// * `queue::sql::grant_production_lease_sql` — `status`
/// * `queue::sql::terminalize_exhausted_production_sql` — `status`,
///   `result_json`, `updated_at`
/// * `transitions::terminalize_sql` — `status`, `terminal_reason`,
///   `result_json`, `fail_type`, `updated_at`
/// * `transitions::record_deadline_adjustments_sql` writes `deadline_adjustments_json`.
///
/// The SELECT side is deliberately NOT column-targeted: `transitions::FENCED_PREFIX`
/// locks the run with `SELECT r.* FROM runs AS r`, which needs SELECT on EVERY
/// column of `runs`. A column list there would withhold nothing and break the
/// fence, so relation-grain SELECT is the measured surface rather than a
/// concession. Confining the WRITE is what a column list can still buy, and it
/// buys a lot: this family matches the permissive `TO wamn_platform` base policy arm,
/// so a blanket `UPDATE` would let one lease rewrite any project's admission pins.
pub const EXECUTOR_PLATFORM_RUN_UPDATE_COLUMNS: [&str; 7] = [
    "status",
    "terminal_reason",
    "fail_type",
    "result_json",
    "deadline_adjustments_json",
    "state_json",
    "updated_at",
];

/// Every `run_queue` column the executor-platform family WRITES.
///
/// `queue::sql::advance_lease_attempts_sql` writes `attempts`;
/// `queue::sql::grant_production_lease_sql` writes `lease_owner`,
/// `lease_expires_at` and `lease_generation`;
/// `queue::sql::renew_production_lease_sql` writes `lease_expires_at`.
/// `queue::sql::release_production_lease_sql` writes `attempts`, `available_at`,
/// `lease_owner` and `lease_expires_at` for credential retries. Nothing writes
/// `max_attempts` or `enqueued_at`.
///
/// SELECT stays relation grain for the same reason it does on `runs`:
/// `transitions::FENCED_PREFIX` locks the queue row with
/// `SELECT q.* FROM run_queue AS q`. DELETE has no column grain in PostgreSQL at
/// all, so the two dequeue statements take it whole.
pub const EXECUTOR_PLATFORM_QUEUE_UPDATE_COLUMNS: [&str; 5] = [
    "lease_owner",
    "lease_expires_at",
    "lease_generation",
    "attempts",
    "available_at",
];

/// Every `runs` column the executor-platform family INSERTs for event, schedule
/// and manual workflow runs.
///
/// The host admits an event or schedule run under the authority of the
/// release that declares the workflow (`wamn_run_state::queue::insert_event_run_sql`).
/// A schedule carries its application service principal. A manual workflow
/// run carries its executing principal and role, its invoker and its frozen
/// node digests (`wamn_run_state::queue::insert_manual_run_sql`). The list
/// reaches no binding world, legacy flow, capture or
/// caller-outcome column.
pub const EXECUTOR_PLATFORM_RUN_INSERT_COLUMNS: [&str; 18] = [
    "package_id",
    "manifest_digest",
    "environment",
    "wiring_id",
    "wiring_version",
    "wiring_hash",
    "trigger_source",
    "execution_principal_id",
    "execution_role",
    "invoked_by",
    "invoked_by_user_id",
    "node_digests_json",
    "idempotency_key",
    "input_json",
    "status",
    "event_source_run_id",
    "event_root_run_id",
    "event_depth",
];

/// The `run_queue` columns an event or schedule admission writes. The FIFO position, the
/// crash budget and the lease take their defaults.
pub const EXECUTOR_PLATFORM_QUEUE_INSERT_COLUMNS: [&str; 1] = ["run_id"];
/// Idempotently create or harden [`PLATFORM_GROUP_ROLE`], the shared NOLOGIN
/// group every non-guest project-base-grants arm targets (`wamn-0h0g.22.17`).
///
/// EXCEPTION-GUARDED, not bare `IF NOT EXISTS`. Roles are CLUSTER-global, so two
/// appliers racing on a cluster where the role is absent both take the `THEN`
/// branch and the loser gets `duplicate_object`. The advisory lock serializes
/// appliers that take it; the handler covers the ones that do not.
///
/// No grants, no ownership, `NOBYPASSRLS`: the role's whole purpose is to be
/// NAMED by a policy's `TO` clause. What a platform family may actually reach
/// stays its own table grants.
pub fn ensure_platform_group_role_sql() -> String {
    ensure_acl_role_sql(PLATFORM_GROUP_ROLE)
}

/// Converge one family's stable ACL role onto (or off) [`PLATFORM_GROUP_ROLE`].
///
/// # `INHERIT TRUE` is spelled, and omitting it fails SILENTLY
///
/// [`ensure_workload_acl_role_sql`] mints every stable ACL role `NOINHERIT`. In
/// PostgreSQL 16+ a role's `rolinherit` supplies the DEFAULT `INHERIT` option
/// for memberships granted TO it, and RLS role matching walks `pg_auth_members`
/// by that PER-EDGE option — so a bare `GRANT wamn_platform TO
/// wamn_run_retention` lands `inherit_option = false` and the two-hop chain
/// (generation login -> stable ACL role -> `wamn_platform`) dies. Nothing
/// raises: the generation still holds its table grants, and the base policy
/// simply matches no row. Measured on PostgreSQL 18.6 against the real files —
/// bare grant reads 0 rows, `INHERIT TRUE` reads all of them.
///
/// `ADMIN FALSE, SET FALSE` mirror
/// [`normalize_workload_generation_membership_sql`]: the edge confers policy
/// membership and nothing else. `REVOKE` precedes the `GRANT` so a replay that
/// finds a drifted edge replaces it instead of leaving it, and so a family that
/// stops being platform-grain loses the arm on the next converge.
/// # It does not ensure the ACL role
///
/// An ACL role's lifecycle belongs to its own builder. This function only
/// touches the membership edge, behind an existence
/// guard, so a caller that has not created the role yet gets a no-op rather than
/// an error.
pub fn platform_group_membership_sql(family: WorkloadRoleFamily) -> String {
    let acl_role = escape_identifier(family.acl_role());
    let acl_role_lit = escape_literal(family.acl_role());
    let group = escape_identifier(PLATFORM_GROUP_ROLE);
    let group_lit = escape_literal(PLATFORM_GROUP_ROLE);
    let edges = format!(
        "SELECT FROM pg_auth_members m \
           JOIN pg_roles g ON g.oid = m.roleid JOIN pg_roles r ON r.oid = m.member \
          WHERE g.rolname = {group_lit} AND r.rolname = {acl_role_lit}"
    );
    // The edge changes only when it differs from the wanted one, so a converged
    // cluster-wide role is not altered, which the provisioner could not do to a
    // role that a superuser made (owner ruling of 2026-10-03 on wamn-zua8.3).
    let (drift, grant) = if family.is_platform_grain() {
        (
            format!(
                "NOT EXISTS ({edges} AND m.inherit_option AND NOT m.set_option \
                                     AND NOT m.admin_option) \
                 OR EXISTS ({edges} AND NOT (m.inherit_option AND NOT m.set_option \
                                             AND NOT m.admin_option))"
            ),
            format!(" GRANT {group} TO {acl_role} WITH ADMIN FALSE, INHERIT TRUE, SET FALSE;"),
        )
    } else {
        (format!("EXISTS ({edges})"), String::new())
    };
    format!(
        "{ensure_group} \
         DO $platform_edge$ BEGIN \
           IF EXISTS (SELECT FROM pg_roles WHERE rolname = {acl_role_lit}) AND ({drift}) THEN \
             REVOKE {group} FROM {acl_role};{grant} \
           END IF; \
         END $platform_edge$;",
        ensure_group = ensure_platform_group_role_sql(),
    )
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
// `deploy/sql/system-schema.sql` declares no policy and forces no row-level
// security, so confinement in this database is PURELY GRANT-SHAPED: there is no
// project predicate to satisfy and none is invented here. The owner credential is
// therefore not merely wide, it is unconfined — it can INSERT an `identity.pats`
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

/// The relations the management and user route identity reads touch.
///
/// `SELECT_PAT_BY_PREFIX_SQL` joins `pats` to `principals`; `SELECT_PROJECT_ROLES_SQL`
/// reads `project_roles`; user routes require `project_env_memberships`
/// (all in `crates/identity/src/lib.rs`).
/// PostgreSQL tests privileges on every relation a statement references, so all
/// relations are required, including password login revocation state.
pub const IDENTITY_READER_RELATIONS: [&str; 5] = [
    "pats",
    "principals",
    "project_env_memberships",
    "project_roles",
    "password_logins",
];

/// The columns the identity reader reads beside [`IDENTITY_READER_RELATIONS`]:
/// session admission tests the active org membership (`wamn-ms1sn.2`).
pub const IDENTITY_READER_COLUMNS: [(&str, &[&str]); 1] =
    [("org_memberships", &["principal_id", "org", "status"])];

/// Non-secret connection functions available to the control host.
pub const CONTROL_CONNECTION_FUNCTIONS: &[&str] = &[
    "publish_integration(text,integer,text,jsonb,text,text[],text[])",
    "configure_integration_type(text,text,jsonb)",
];

/// The privileges the control host holds on a relation that it writes.
const CONTROL_WRITE: &[&str] = &["SELECT", "INSERT", "UPDATE", "DELETE"];

/// The control database relations an org's control host reaches, with the
/// privileges it holds on each (docs/architecture/execution.md#host-run-routes,
/// wamn-a40n.3). It reads who is a user and whether the password login of a
/// control session is live. It writes the org memberships and the org, project
/// and environment grants of the org operations, and it reads the projects and
/// environments those grants cover. The `project.create` route writes a
/// project. It also stores encrypted OAuth tokens and pending authorizations.
pub const CONTROL_SURFACE: [(&str, &str, &[&str]); 15] = [
    ("identity", "org_memberships", CONTROL_WRITE),
    ("identity", "org_roles", CONTROL_WRITE),
    ("identity", "environment_roles", CONTROL_WRITE),
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
    (
        "registry",
        "connection_principal",
        &["SELECT", "INSERT", "DELETE"],
    ),
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
        _ => None,
    }
}

/// Record the owner-maintained login-identity-to-project mapping for one target.
///
/// `$1` login identity, `$2` `project_env_key`, `$3` org, `$4` project, `$5` environment.
/// Each author-accessed relation resolves project authority through this row.
/// The statement belongs to the database owner and carries no
/// grant to the author. `DO UPDATE` is restricted to a row whose project already
/// matches, so a replay converges while a re-point to a different project returns
/// **zero rows** instead of widening one login's reach — the caller must read an
/// empty result as a refusal, not as a no-op.
pub fn upsert_control_author_project_env_key_mapping_sql() -> &'static str {
    "INSERT INTO wamn_authority.author_login_projects \
       (login_identity, project_env_key, org_id, project_id, environment) \
     VALUES ($1, $2, $3, $4, $5) \
     ON CONFLICT (login_identity) DO UPDATE \
       SET org_id = EXCLUDED.org_id, \
           project_id = EXCLUDED.project_id, \
           environment = EXCLUDED.environment \
       WHERE author_login_projects.project_env_key = EXCLUDED.project_env_key \
     RETURNING project_env_key"
}

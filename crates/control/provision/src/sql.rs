//! Pure Postgres text builders for project provisioning (SR3 house rule 3:
//! pure text + validated/quoted identifiers; the driver holds the connection).
//!
//! Every builder takes an **already-validated** name (see
//! [`crate::validate_project_env`]); a database name it takes is
//! double-quoted, so a slug (which cannot contain a `"`) is injection-safe. The
//! `wamn_app` role name is a pinned constant. Values that vary (a probed role
//! name, a role password) travel as `$n` params or quoted literals.

use crate::tenant_key::{TENANT_KEY_FUNCTION, TENANT_KEY_SCHEMA};

mod cdc;
mod credentials;
mod database;
mod database_grants;
mod package_owner;

#[doc(inline)]
use std::fmt::Write as _;

pub use database::{
    PLATFORM_EXTENSIONS, create_database_named_sql, drop_database_named_sql,
    ensure_db_owner_role_sql, install_platform_extensions_sql, set_database_owner_sql,
};

pub(crate) use credentials::{
    ensure_acl_role_sql, retire_acl_generation_sql, revoke_superuser_sql,
};
#[doc(inline)]
pub use credentials::{
    ensure_workload_acl_role_sql, forget_generation_password_sql, generation_password_recorded_sql,
    normalize_workload_generation_membership_sql, prepare_workload_generation_sql,
    record_generation_password_sql, retire_workload_generation_sql, system_generation_state_sql,
    terminate_workload_generation_sessions_sql, workload_generation_state_sql,
    workload_scope_lock_sql,
};

#[doc(inline)]
pub use database_grants::{
    non_template_databases_sql, public_connect_databases_sql,
    public_temporary_on_current_database_sql, revoke_public_connect_floor_sql,
    role_database_grants_sql,
};

#[doc(inline)]
pub use cdc::{
    CDC_PUBLISH, PUBLICATION_PROBE_SQL, create_failover_slot_sql, create_publication_schemas_sql,
    create_publication_sql, drop_publication_sql, drop_replication_slot_sql,
    ensure_cdc_exclusion_map_sql, ensure_entity_map_sql, ensure_replication_role_sql,
    ensure_schema_sql, grant_replication_access_sql, upsert_cdc_exclusion_map_sql,
    upsert_entity_map_sql,
};

pub use package_owner::{ensure_package_owner_role_sql, package_owner_grants_sql};

use crate::name::{APP_ROLE, DB_OWNER_ROLE};
use crate::workload_role::{
    ADMINISTRATION_ROLE, EVENT_MATERIALIZER_ROLE, EXECUTOR_PLATFORM_ROLE, HTTP_ADMITTER_ROLE,
    PLATFORM_GROUP_ROLE, SESSION_ROLE_READER_ROLE, WorkloadRoleFamily,
};
pub(crate) use postgres_protocol::escape::escape_identifier;
use postgres_protocol::escape::escape_literal;

/// Insert the declared tenant identity without replacing an existing row.
pub fn insert_env_key_environment_sql() -> &'static str {
    "INSERT INTO catalog.tenant_environments \
     (env_key, org, project, env, instance_suffix, disposable, environment_instance) \
     VALUES ($1, $2, $3, $4, $5, $6, '') ON CONFLICT (env_key) DO NOTHING"
}

/// Lock and read the winning identity after the insert attempt.
pub fn read_env_key_environment_sql() -> &'static str {
    "SELECT org, project, env FROM catalog.tenant_environments WHERE env_key = $1 FOR UPDATE"
}

/// Refresh the declared values and clear the old instance after Rust accepts the identity.
pub fn refresh_env_key_environment_sql() -> &'static str {
    "UPDATE catalog.tenant_environments SET instance_suffix = $2, disposable = $3, \
     environment_instance = '', projected_at = now() WHERE env_key = $1"
}

/// Idempotently create or harden the shared, cluster-global [`APP_ROLE`] as a
/// stable NOLOGIN ACL role.
///
/// Guest sessions authenticate as scoped [`WorkloadRoleFamily::App`]
/// generations and inherit this role's grants. The stable role therefore
/// carries no verifier and cannot authenticate or inherit memberships itself.
/// The shared bootstrap lock serializes cooperative appliers; the exception arm
/// hardens the role created by a concurrent non-cooperative applier. Callers
/// must commit this statement before applying
/// [`drain_app_role_sessions_sql`]. That boundary prevents a new shared-role
/// login from racing the drain while `NOLOGIN` is still uncommitted.
pub fn ensure_app_acl_role_sql() -> String {
    format!(
        "DO $app_role$ BEGIN \
           PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); \
           IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = {role_lit}) THEN \
             CREATE ROLE {role} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \
           ELSIF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = {role_lit} \
                         AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole \
                              OR rolinherit OR rolreplication OR rolbypassrls)) THEN \
             ALTER ROLE {role} NOLOGIN PASSWORD NULL NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
             {nosuperuser} \
           END IF; \
         EXCEPTION WHEN duplicate_object THEN \
           ALTER ROLE {role} NOLOGIN PASSWORD NULL NOCREATEDB \
             NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
           {nosuperuser} \
         END $app_role$;",
        role = escape_identifier(APP_ROLE),
        role_lit = escape_literal(APP_ROLE),
        nosuperuser = revoke_superuser_sql(&escape_literal(APP_ROLE)),
    )
}

/// Drain sessions authenticated as the retired shared [`APP_ROLE`].
///
/// Apply only after [`ensure_app_acl_role_sql`] has committed. PostgreSQL's
/// native timeout bounds each termination wait; the final session list turns a
/// refused or timed-out termination into a hard failure instead of silently
/// returning with an old credential session alive.
pub fn drain_app_role_sessions_sql() -> String {
    format!(
        "DO $app_drain$ DECLARE stale_pid integer; BEGIN \
           FOR stale_pid IN SELECT pid FROM pg_catalog.pg_stat_activity \
             WHERE usename = {role_lit} AND pid <> pg_backend_pid() LOOP \
             PERFORM pg_catalog.pg_terminate_backend(stale_pid, 5000); \
           END LOOP; \
           PERFORM pg_catalog.pg_stat_clear_snapshot(); \
           IF EXISTS (SELECT FROM pg_catalog.pg_stat_activity \
                      WHERE usename = {role_lit} AND pid <> pg_backend_pid()) THEN \
             RAISE EXCEPTION USING ERRCODE = '55000', \
               MESSAGE = 'wamn-app-session-drain-incomplete'; \
           END IF; \
         END $app_drain$;",
        role_lit = escape_literal(APP_ROLE),
    )
}

/// Catalog relations read by callable-HTTP connection authorization.
///
/// `CONNECTION_EFFECT_SNAPSHOT_SQL` resolves one exact package membership and
/// joins the requirement, binding, instance, and generation facts, and the
/// components that the release manifest carries. It reads the workflow graph
/// through [`HTTP_ADMITTER_WORKFLOW_COLUMNS`], not through this list. Unlike
/// management admission, it never consults the environment's mutable
/// effective-release head.
pub const HTTP_ADMITTER_CATALOG_RELATIONS: [&str; 4] = [
    "releases",
    "connection_requirement",
    "connection_binding",
    "release_connection",
];

/// The `catalog.workflows` columns that callable-HTTP connection authorization
/// reads: the key of one workflow version and its graph. The admitter gets
/// these columns only, not the whole table.
pub const HTTP_ADMITTER_WORKFLOW_COLUMNS: [&str; 6] = [
    "env_key",
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
/// check expects exactly these grants, so the two cannot drift.
pub fn http_admitter_relations() -> impl Iterator<Item = (&'static str, &'static str)> {
    HTTP_ADMITTER_CATALOG_RELATIONS
        .iter()
        .map(|relation| ("catalog", *relation))
        .chain([
            ("platform", "connection"),
            ("platform", "integration"),
            ("platform", "permission"),
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
/// * `queue::sql::grant_production_claim_sql` — `status`
/// * `queue::sql::terminalize_exhausted_production_sql` — `status`,
///   `result_json`, the caller-outcome family, `updated_at`
/// * `transitions::release_caller_sql` — the caller-outcome family, `updated_at`
/// * `transitions::terminalize_sql` — `status`, `terminal_reason`,
///   `result_json`, `fail_type`, `updated_at`
/// * `transitions::record_deadline_adjustments_sql` writes `deadline_adjustments_json`.
///
/// The SELECT side is deliberately NOT column-scoped: `transitions::FENCED_PREFIX`
/// locks the run with `SELECT r.* FROM runs AS r`, which needs SELECT on EVERY
/// column of `runs`. A column list there would withhold nothing and break the
/// fence, so relation-grain SELECT is the measured surface rather than a
/// concession. Confining the WRITE is what a column list can still buy, and it
/// buys a lot: this family matches the permissive `TO wamn_platform` floor arm,
/// so a blanket `UPDATE` would let one claim rewrite any tenant's admission pins.
pub const EXECUTOR_PLATFORM_RUN_UPDATE_COLUMNS: [&str; 13] = [
    "status",
    "terminal_reason",
    "fail_type",
    "result_json",
    "deadline_adjustments_json",
    "state_json",
    "caller_outcome_type",
    "caller_outcome_json",
    "caller_http_status",
    "caller_release_node_id",
    "caller_outcome_hash",
    "caller_released_at",
    "updated_at",
];

/// Every `run_queue` column the executor-platform family WRITES.
///
/// `queue::sql::advance_claim_attempts_sql` writes `attempts`;
/// `queue::sql::grant_production_claim_sql` writes `lease_owner`,
/// `lease_expires_at` and `lease_generation`;
/// `queue::sql::renew_production_lease_sql` writes `lease_expires_at`.
/// `queue::sql::release_production_claim_sql` writes `attempts`, `available_at`,
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
pub const EXECUTOR_PLATFORM_RUN_INSERT_COLUMNS: [&str; 21] = [
    "env_key",
    "package_id",
    "manifest_digest",
    "environment",
    "wiring_id",
    "wiring_version",
    "wiring_hash",
    "trigger_source",
    "registration_id",
    "service_principal_id",
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
pub const EXECUTOR_PLATFORM_QUEUE_INSERT_COLUMNS: [&str; 2] = ["env_key", "run_id"];
/// Idempotently create or harden [`PLATFORM_GROUP_ROLE`], the shared NOLOGIN
/// group every non-guest tenant-floor arm targets (`wamn-0h0g.22.17`).
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
    format!(
        "DO $platform_group$ DECLARE role_name text := {role_lit}; BEGIN \
           PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN \
             EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
           ELSIF EXISTS (SELECT FROM pg_roles WHERE rolname = role_name \
                         AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole \
                              OR rolinherit OR rolreplication OR rolbypassrls)) THEN \
             EXECUTE format('ALTER ROLE %I NOLOGIN PASSWORD NULL NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
             {nosuperuser} \
           END IF; \
         EXCEPTION WHEN duplicate_object THEN NULL; \
         END $platform_group$;",
        role_lit = escape_literal(PLATFORM_GROUP_ROLE),
        nosuperuser = revoke_superuser_sql("role_name"),
    )
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
/// raises: the generation still holds its table grants, and the floor policy
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

/// `GRANT EXECUTE` on the tenant-key derivation for one stable ACL role.
///
/// # A WRITER needs this, not only a reader of the predicate
///
/// Every governed relation carries a `<table>_tkey` EXPRESSION INDEX over
/// `wamn_authority.tenant_key(text)`, and PostgreSQL evaluates that expression
/// while inserting or updating the row. So a family holding a COMPLETE table
/// grant set and the `wamn_platform` floor arm still fails its INSERT with
/// 42501 `permission denied for function tenant_key` unless it holds this
/// (`wamn-0h0g.22.28`). Measured on PostgreSQL 18.6 as a login asserted NOT
/// (`rolsuper` OR `rolbypassrls`): with the grant the row lands, without it the
/// statement raises 42501 and the post-state is zero rows.
///
/// # Exactly this, and nothing beside it
///
/// * NOT `USAGE` on `wamn_authority`. An index expression is a stored, already
///   resolved node tree, so no name resolution happens at insert time and only
///   the function's EXECUTE ACL is checked. Measured: EXECUTE alone, with
///   `has_schema_privilege(role, 'wamn_authority', 'USAGE') = false`, inserts.
/// * NOT `current_tenant_key()`. That derivation is called only by the tenant
///   floor policy, which `wamn-0h0g.22.17` narrowed `TO wamn_app`; a non-guest
///   family matches the permissive `TO wamn_platform` arm, whose `USING (true)`
///   calls nothing. Measured: the INSERT above succeeds with
///   `has_function_privilege(role, 'wamn_authority.current_tenant_key()',
///   'EXECUTE') = false`.
/// * NOT to [`PLATFORM_GROUP_ROLE`]. That group carries POLICY MEMBERSHIP only
///   and holds no privilege of its own; granting function EXECUTE to it would
///   hand the derivation to every member, including the families that never
///   write. It is granted here, beside the one family's own grant set, so the
///   surface stays the measured production surface.
fn grant_tenant_key_execute_sql(role: &str) -> String {
    format!(
        "GRANT EXECUTE ON FUNCTION {schema}.tenant_key(text) TO {role};",
        schema = escape_identifier(crate::tenant_key::TENANT_KEY_SCHEMA),
        role = escape_identifier(role),
    )
}

fn quoted_column_list(columns: &[&str]) -> String {
    columns
        .iter()
        .map(|column| escape_identifier(column))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Converge the stable callable-HTTP admitter role to its exact read surface
/// (`wamn-0h0g.22.37`).
///
/// The connection-effect snapshot and fresh operation-permission reads use
/// [`HTTP_ADMITTER_CATALOG_RELATIONS`] and the three platform relations of
/// [`http_admitter_relations`].
/// User permission reads join the active application user to its role grants;
/// service permission reads continue to read `platform.permission` alone.
/// A read route's ETag reads the WAL position without a table grant.
/// `USAGE` on those two schemas plus eight `SELECT`s and the column `SELECT`
/// of [`HTTP_ADMITTER_WORKFLOW_COLUMNS`] on `catalog.workflows` is the
/// family's authority.
///
/// # What it deliberately does NOT hold
///
/// * NO `wamn_run` anything. The admitter authorizes an effect; it does not
///   admit, claim, lease or terminalize a run. `schema` is therefore the REVOKE
///   scope and never a grant scope — an environment where someone handed this
///   family a run-plane privilege converges back on the next apply.
/// * NO write of any grain, so no `wamn_authority.tenant_key(text)` EXECUTE
///   either. That grant exists for the families whose INSERT or UPDATE makes
///   PostgreSQL evaluate a `<table>_tkey` expression index; a pure reader never
///   forms an index entry, and these statements carry no predicate over the
///   derivation for the planner to evaluate.
/// * NO function EXECUTE at all. The statement calls only built-ins.
///
/// Run connected to the project-environment database as a principal owning both
/// schemas (the database owner or the cluster superuser), AFTER
/// `wamn_catalog::CATALOG_SCHEMA_SQL` and the run-plane files have been applied —
/// the `REVOKE`/`GRANT` name relations that must already exist.
pub fn grant_http_admitter_surface_sql(schema: &str) -> String {
    let role = escape_identifier(HTTP_ADMITTER_ROLE);
    let schema = escape_identifier(schema);
    let schemas = HTTP_ADMITTER_SCHEMAS.join(", ");
    let mut sql = format!(
        "{ensure} \
         REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA {schemas}, {schema} \
         FROM {role}; \
         REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA {schemas}, {schema} \
         FROM {role}; \
         REVOKE ALL PRIVILEGES ON SCHEMA {schemas}, {schema} FROM {role}; \
         GRANT USAGE ON SCHEMA {schemas} TO {role};",
        ensure = ensure_workload_acl_role_sql(WorkloadRoleFamily::HttpAdmitter),
    );
    for (schema, relation) in http_admitter_relations() {
        write!(
            sql,
            " GRANT SELECT ON TABLE {schema}.{relation} TO {role};",
            relation = escape_identifier(relation),
        )
        .expect("writing to a String cannot fail");
    }
    let columns = HTTP_ADMITTER_WORKFLOW_COLUMNS
        .iter()
        .map(|column| escape_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    write!(
        sql,
        " GRANT SELECT ({columns}) ON TABLE catalog.\"workflows\" TO {role};"
    )
    .expect("writing to a String cannot fail");
    sql
}

/// The application authorization relations the administration family reads and
/// writes (docs/architecture/data-access.md#actors), the environment status
/// that the status routes mirror
/// (docs/architecture/execution.md#host-run-routes), and the platform
/// configuration tables that the control host writes
/// (docs/architecture/data-access.md#actors). `number_counter` is not one of
/// them: only `platform.bump_counter`, `platform.assign_counter` and
/// `platform.set_counter` write it.
pub const ADMINISTRATION_RELATIONS: [&str; 11] = [
    "user",
    "role",
    "user_role",
    "permission",
    "environment",
    "external_system",
    "connection",
    "file",
    "integration",
    "number_sequence",
    "setting",
];

/// Converge the stable administration role to its exact surface
/// (`wamn-a40n.2`).
///
/// The role reads and writes [`ADMINISTRATION_RELATIONS`]. The grant goes to
/// this one family, not to [`PLATFORM_GROUP_ROLE`], which every platform
/// family inherits (owner correction of 2026-09-30). A write of one of them
/// also appends the entry of its `<table>_history` table and renders it
/// through `wamn_history.row_image` with the writer's authority, so the role holds `INSERT` on the entry
/// columns of those history tables, `USAGE` on `wamn_history`, `EXECUTE` on
/// `row_image`. On the catalog it
/// reads `catalog.releases` only, for the control host's `release.profile`.
/// It holds nothing on the run plane. `schema` is the run-plane revoke scope.
pub fn grant_administration_surface_sql(schema: &str) -> String {
    let role = escape_identifier(ADMINISTRATION_ROLE);
    let schema = escape_identifier(schema);
    let mut sql = format!(
        "{ensure} \
         REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, platform, {schema} FROM {role}; \
         REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, platform, wamn_history, {schema} \
         FROM {role}; \
         REVOKE ALL PRIVILEGES ON SCHEMA catalog, platform, wamn_history, {schema} FROM {role}; \
         REVOKE EXECUTE ON FUNCTION {TENANT_KEY_FUNCTION}(text) FROM {role}; \
         GRANT USAGE ON SCHEMA catalog, platform, wamn_history TO {role}; \
         GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO {role}; \
         GRANT SELECT ON TABLE catalog.releases TO {role}; \
         GRANT SELECT ON TABLE catalog.connection_requirement, catalog.release_connection, catalog.connection_binding TO {role};",
        ensure = ensure_workload_acl_role_sql(WorkloadRoleFamily::Administration),
    );
    for relation in ADMINISTRATION_RELATIONS {
        write!(
            sql,
            " GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE platform.{} TO {role};",
            escape_identifier(relation)
        )
        .expect("writing to a String cannot fail");
        if !["external_system", "integration", "connection"].contains(&relation) {
            write!(sql, " GRANT INSERT (row_key, type, operation, changed_by, changed_at, transaction_id, before, after) ON TABLE platform.{} TO {role};", escape_identifier(&format!("{relation}_history")))
                .expect("writing to a String cannot fail");
        }
    }

    sql
}

/// Converge the session role reader onto four columns and no execution authority.
///
/// Apply after the project schemas exist. The connection selects the environment database for these reads. Revoke column
/// ACLs as well as table ACLs so replay removes a widened read or write.
pub fn grant_session_role_reader_surface_sql() -> String {
    let role = escape_identifier(SESSION_ROLE_READER_ROLE);
    format!(
        "{ensure} \
         REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, platform, wamn_run, {TENANT_KEY_SCHEMA} FROM {role}; \
         REVOKE ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA catalog, platform, wamn_run, {TENANT_KEY_SCHEMA} FROM {role}; \
         REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, platform, wamn_run, {TENANT_KEY_SCHEMA} FROM {role}; \
         DO $session_column_acl$ DECLARE target record; BEGIN \
           FOR target IN \
             SELECT n.nspname, c.relname, a.attname \
               FROM pg_catalog.pg_attribute a \
               JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
               JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
              WHERE n.nspname IN ('catalog', 'platform', 'wamn_run', '{TENANT_KEY_SCHEMA}') \
                AND c.relkind IN ('r', 'p', 'v', 'm', 'f') \
                AND a.attnum > 0 AND NOT a.attisdropped AND a.attacl IS NOT NULL \
           LOOP \
             EXECUTE format('REVOKE ALL PRIVILEGES (%I) ON TABLE %I.%I FROM %I', \
               target.attname, target.nspname, target.relname, {role_literal}); \
           END LOOP; \
         END $session_column_acl$; \
         REVOKE ALL PRIVILEGES ON SCHEMA catalog, platform, wamn_run, {TENANT_KEY_SCHEMA} FROM {role}; \
         GRANT USAGE ON SCHEMA platform TO {role}; \
         GRANT SELECT (id, status) ON TABLE platform.\"user\" TO {role}; \
         GRANT SELECT (user_id, role_name) ON TABLE platform.user_role TO {role};",
        ensure = ensure_workload_acl_role_sql(WorkloadRoleFamily::SessionRoleReader),
        role_literal = escape_literal(SESSION_ROLE_READER_ROLE),
    )
}

/// Converge the stable executor-platform role to its exact claim surface
/// (`wamn-0h0g.22.37`).
///
/// The production queries that select `AuthorityClass::ExecutorPlatform` live
/// in `wamn_runtime::plugins::wamn_postgres::production_claim` and its
/// `wiring_resolution` sibling.
///
/// # The grains, and why each is the one it is
///
/// * `catalog`: `USAGE` plus TABLE-grain `SELECT` on
///   [`EXECUTOR_PLATFORM_CATALOG_RELATIONS`]. No write of any type.
/// * `runs`: TABLE-grain `SELECT` because `transitions::FENCED_PREFIX` locks the
///   row with `SELECT r.*`, and COLUMN-grain `UPDATE` over
///   [`EXECUTOR_PLATFORM_RUN_UPDATE_COLUMNS`]. The admission pins, the wiring
///   identity and the input cannot be updated by this role. The shared
///   `TO wamn_platform` policy uses `USING (true)`, so a table-wide `UPDATE`
///   would let the role rewrite other environments.
/// * `run_queue`: TABLE-grain `SELECT` (`SELECT q.*` in the same fence) and
///   `DELETE` (PostgreSQL has no column-grain DELETE), plus COLUMN-grain
///   `UPDATE` over [`EXECUTOR_PLATFORM_QUEUE_UPDATE_COLUMNS`] — the lease and
///   the crash counter, never the FIFO position.
/// * `runs` and `run_queue` INSERT, column grain over
///   [`EXECUTOR_PLATFORM_RUN_INSERT_COLUMNS`] and
///   [`EXECUTOR_PLATFORM_QUEUE_INSERT_COLUMNS`], plus `SELECT` on
///   `environment_policies`: the host admits event and schedule runs
///   under the declaring release. Schedules carry their application identity.
/// * `wamn_authority.tenant_key(text)`: MEASURED, and not optional. `runs`
///   carries the `runs_tkey` EXPRESSION INDEX over that function, and
///   PostgreSQL evaluates an index expression while forming the new index entry
///   an UPDATE requires. Four of the six `runs` writes change `status`, which
///   participates in the partial-index predicates on `runs`, so those updates
///   can never take the HOT path that would skip index maintenance: the
///   evaluation — and therefore the EXECUTE check — is unconditional for them,
///   exactly as it is for the management admitter's INSERT (`wamn-0h0g.22.28`).
///   Schema `USAGE` on `wamn_authority` is NOT granted and is NOT needed: an
///   index expression is a stored, already-resolved node tree, so no name
///   resolution happens and only the function's EXECUTE ACL is checked.
///
/// Blanket revocation over BOTH schemas precedes every grant, so a replay
/// NARROWS as well as grants. A table-level `REVOKE` carries that table's
/// column-level ACL entries with it, which is what makes the column-exact
/// `UPDATE` sets convergent rather than merely additive.
pub fn grant_executor_platform_surface_sql(schema: &str) -> String {
    let role = escape_identifier(EXECUTOR_PLATFORM_ROLE);
    let schema = escape_identifier(schema);
    let mut sql = format!(
        "{ensure} \
         REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, platform, {schema} FROM {role}; \
         REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, platform, {schema} FROM {role}; \
         REVOKE ALL PRIVILEGES ON SCHEMA catalog, platform, {schema} FROM {role}; \
         GRANT USAGE ON SCHEMA catalog, platform, {schema} TO {role};",
        ensure = ensure_workload_acl_role_sql(WorkloadRoleFamily::ExecutorPlatform),
    );
    for relation in EXECUTOR_PLATFORM_CATALOG_RELATIONS {
        write!(
            sql,
            " GRANT SELECT ON TABLE catalog.{relation} TO {role};",
            relation = escape_identifier(relation),
        )
        .expect("writing to a String cannot fail");
    }
    for relation in ["connection", "integration"] {
        write!(
            sql,
            " GRANT SELECT ON TABLE platform.{} TO {role};",
            escape_identifier(relation)
        )
        .expect("writing to a String cannot fail");
    }
    let run_update = quoted_column_list(&EXECUTOR_PLATFORM_RUN_UPDATE_COLUMNS);
    let queue_update = quoted_column_list(&EXECUTOR_PLATFORM_QUEUE_UPDATE_COLUMNS);
    let run_insert = quoted_column_list(&EXECUTOR_PLATFORM_RUN_INSERT_COLUMNS);
    let queue_insert = quoted_column_list(&EXECUTOR_PLATFORM_QUEUE_INSERT_COLUMNS);
    write!(
        sql,
        " GRANT SELECT ON TABLE {schema}.\"runs\" TO {role}; \
         GRANT UPDATE ({run_update}) ON TABLE {schema}.\"runs\" TO {role}; \
         GRANT INSERT ({run_insert}) ON TABLE {schema}.\"runs\" TO {role}; \
         GRANT SELECT, DELETE ON TABLE {schema}.\"run_queue\" TO {role}; \
         GRANT UPDATE ({queue_update}) ON TABLE {schema}.\"run_queue\" TO {role}; \
         GRANT INSERT ({queue_insert}) ON TABLE {schema}.\"run_queue\" TO {role}; \
         GRANT SELECT ON TABLE {schema}.\"environment_policies\" TO {role}; \
         GRANT SELECT (id, type, status, email) ON TABLE platform.\"user\" TO {role};"
    )
    .expect("writing to a String cannot fail");
    // `runs` carries `runs_tkey`, so the column-exact UPDATE above is dead
    // without this on every write that changes an indexed column.
    sql.push(' ');
    sql.push_str(&grant_tenant_key_execute_sql(EXECUTOR_PLATFORM_ROLE));
    sql.push_str(&grant_executor_history_reads_sql());
    sql
}

/// Grant event admission the entity maps and actor lookup columns of model histories.
/// Run again after package history tables are installed.
pub fn grant_executor_history_reads_sql() -> String {
    format!(
        " DO $event_history$ DECLARE s record; h record; BEGIN \
         IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = {role_name}) THEN RETURN; END IF; \
         FOR s IN SELECT n.nspname FROM pg_namespace n JOIN pg_class c ON c.relnamespace = n.oid \
             WHERE c.relname = 'wamn_entities' AND c.relkind = 'r' LOOP \
           EXECUTE format('GRANT USAGE ON SCHEMA %I TO {role}', s.nspname); \
           EXECUTE format('GRANT SELECT ON TABLE %I.wamn_entities TO {role}', s.nspname); \
           FOR h IN EXECUTE format('SELECT DISTINCT c.relname FROM %I.wamn_entities m \
               JOIN pg_class c ON c.relnamespace = %L::regnamespace \
               AND c.relname = m.table_name || ''_history''', s.nspname, s.nspname) LOOP \
             EXECUTE format('GRANT SELECT (row_key, type, transaction_id, position, changed_by) ON TABLE %I.%I TO {role}', s.nspname, h.relname); \
           END LOOP; \
         END LOOP; END $event_history$;",
        role = escape_identifier(EXECUTOR_PLATFORM_ROLE),
        role_name = escape_literal(EXECUTOR_PLATFORM_ROLE),
    )
}

/// The materializer's complete production read surface: the installed package
/// identities used for source validation. It holds no catalog write,
/// run-plane grant, or routine grant.
pub const EVENT_MATERIALIZER_CATALOG_RELATIONS: [&str; 1] = ["packages"];

/// Converge the stable event-materializer role to its exact package catalog
/// read surface. Revocation spans every authority-bearing project schema so an
/// out-of-band widening is removed on replay.
pub fn grant_event_materializer_surface_sql(schema: &str) -> String {
    let role = escape_identifier(EVENT_MATERIALIZER_ROLE);
    let schema = escape_identifier(schema);
    let mut sql = format!(
        "{ensure} \
         REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, platform, {TENANT_KEY_SCHEMA}, {schema} FROM {role}; \
         REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, platform, {TENANT_KEY_SCHEMA}, {schema} FROM {role}; \
         REVOKE ALL PRIVILEGES ON SCHEMA catalog, platform, {TENANT_KEY_SCHEMA}, {schema} FROM {role}; \
         GRANT USAGE ON SCHEMA catalog TO {role};",
        ensure = ensure_workload_acl_role_sql(WorkloadRoleFamily::EventMaterializer),
    );
    for relation in EVENT_MATERIALIZER_CATALOG_RELATIONS {
        write!(
            sql,
            " GRANT SELECT ON TABLE catalog.{relation} TO {role};",
            relation = escape_identifier(relation),
        )
        .expect("writing to a String cannot fail");
    }
    sql
}

// --- T1 control-database read surfaces (wamn-0h0g.12.116, wamn-0h0g.12.67) ---
//
// The T1 system database's two read-only consumers both authenticated as
// `wamn_system`, the AUTHORIZATION owner of all three schemas below.
// `deploy/sql/system-schema.sql` declares no policy and forces no row-level
// security, so confinement in this database is PURELY GRANT-SHAPED: there is no
// tenant predicate to satisfy and none is invented here. The owner credential is
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
/// The REVOKE scope, not the grant scope: a system-plane reader is revoked
/// across all three and granted inside exactly one.
pub const SYSTEM_PLANE_SCHEMAS: [&str; 3] = ["identity", "provisioning", "registry"];

/// The two relations the CDC reader's `SELECT`s touch.
///
/// `services/cdc-reader` runs `wamn_control_registry::sql::select_event_reader_sql`
/// and `select_capture_gap_sql` over `WAMN_SYSTEM_URL` and nothing else, so this
/// is its whole authority (`wamn-59z6`: the reader reads its gap row).
pub const REGISTRY_READER_RELATIONS: [&str; 2] = ["event_readers", "capture_gap"];

/// The relations the management and user route identity reads touch.
///
/// `SELECT_PAT_BY_PREFIX_SQL` joins `pats` to `principals`; `SELECT_PROJECT_ROLES_SQL`
/// reads `project_roles`; user routes require `project_env_memberships`
/// (all in `crates/identity/platform/src/lib.rs`).
/// PostgreSQL checks privileges on every relation a statement references, so all
/// relations are required, including password login revocation state.
pub const IDENTITY_READER_RELATIONS: [&str; 5] = [
    "pats",
    "principals",
    "project_env_memberships",
    "project_roles",
    "password_logins",
];

/// The columns the identity reader reads beside [`IDENTITY_READER_RELATIONS`]:
/// session admission checks the active org membership (`wamn-ms1sn.2`).
pub const IDENTITY_READER_COLUMNS: [(&str, &[&str]); 1] =
    [("org_memberships", &["principal_id", "org", "status"])];

/// Converge one system-plane reader's stable ACL role to exactly `SELECT` on
/// `relations` inside `schema`, and nothing anywhere else in the control plane.
///
/// Table-level rather than column-exact: these roles hold no write privilege, so a
/// column list would withhold only timestamps and labels while making a query
/// that reads one more column fail in production instead of at review.
///
/// Run connected to the control database as a principal that owns the system
/// schemas or the cluster superuser, AFTER `deploy/sql/system-schema.sql` has
/// been applied — the `REVOKE`/`GRANT` name relations that must already exist.
/// `CREATE ROLE` needs an admin principal, so the whole batch is an admin batch
/// exactly as the other families' are.
fn grant_system_reader_surface_sql(
    family: WorkloadRoleFamily,
    schema: &str,
    relations: &[&str],
) -> String {
    debug_assert!(
        SYSTEM_PLANE_SCHEMAS.contains(&schema),
        "a system-plane reader grants inside a system-plane schema"
    );
    let role = escape_identifier(family.acl_role());
    let plane = SYSTEM_PLANE_SCHEMAS
        .iter()
        .map(|name| escape_identifier(name))
        .collect::<Vec<_>>()
        .join(", ");
    let schema = escape_identifier(schema);
    let mut sql = format!(
        "{ensure} \
         REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA {plane} FROM {role}; \
         REVOKE ALL PRIVILEGES ON SCHEMA {plane} FROM {role}; \
         GRANT USAGE ON SCHEMA {schema} TO {role};",
        ensure = ensure_workload_acl_role_sql(family),
    );
    for relation in relations {
        write!(
            sql,
            " GRANT SELECT ON TABLE {schema}.{relation} TO {role};",
            relation = escape_identifier(relation),
        )
        .expect("writing to a String cannot fail");
    }
    sql
}

/// Converge `wamn_registry_reader` to its exact CDC-registration read surface.
///
/// `SELECT` on `registry.event_readers` and `registry.capture_gap`. The role is refused on `identity.*`
/// by the same batch that grants it here, which is why the CDC reader's
/// credential cannot be reused to read or forge identity.
pub fn grant_registry_reader_surface_sql() -> String {
    grant_system_reader_surface_sql(
        WorkloadRoleFamily::RegistryReader,
        "registry",
        &REGISTRY_READER_RELATIONS,
    )
}

/// Converge `wamn_identity_reader` to its exact identity read surface.
///
/// `SELECT` on four `identity` relations, and NO `INSERT` and NO `UPDATE`
/// anywhere. The consuming statements are plain reads that take no row lock, so
/// a genuinely SELECT-only role serves the surface as it stands.
///
/// **SELECT-only is correct only because `POST /login` no longer exists.** The
/// live cluster role carries `SELECT, INSERT, UPDATE`; its `INSERT` was added
/// deliberately when that route did exist, and the grant then drifted
/// SELECT-only -> +INSERT -> +INSERT,UPDATE out of band THREE times.
/// the identity service’s
/// `only_the_mounted_types_have_a_route_and_the_rest_answer_a_bare_501` decides
/// the mounted route list by CALLING route selection, so a re-mounted session
/// route changes an answer that test reads. The write privilege such a route
/// would need is pinned separately by
/// `the_identity_reader_grants_no_insert_and_no_update`, so the fourth drift
/// fails a test instead of landing.
pub fn grant_identity_reader_surface_sql() -> String {
    let mut sql = grant_system_reader_surface_sql(
        WorkloadRoleFamily::IdentityReader,
        "identity",
        &IDENTITY_READER_RELATIONS,
    );
    for (relation, columns) in IDENTITY_READER_COLUMNS {
        write!(
            sql,
            " GRANT SELECT ({}) ON identity.{relation} TO wamn_identity_reader;",
            columns.join(", "),
        )
        .expect("writing to a String cannot fail");
    }
    sql
}

/// Grant the host its environment readers and encrypted OAuth storage.
pub fn grant_connection_reader_surface_sql() -> String {
    let mut sql =
        grant_system_reader_surface_sql(WorkloadRoleFamily::ConnectionReader, "registry", &[]);
    let role = escape_identifier(WorkloadRoleFamily::ConnectionReader.acl_role());
    write!(
        sql,
        " REVOKE ALL PRIVILEGES ON ALL FUNCTIONS IN SCHEMA registry, identity FROM {role}; GRANT EXECUTE ON FUNCTION registry.lock_oauth_connection(text,text,text,uuid), registry.read_file_connection(text,text,text,uuid), registry.read_connection_fact(text,text,text,uuid), registry.read_connection_generation(text,text,text,uuid,bigint), registry.read_connection_secret(text,text,text,uuid), registry.read_env_connections(text,text,text), registry.read_integration_version(text,integer) TO {role};"
    )
    .expect("writing to a String cannot fail");
    write!(sql, " GRANT USAGE ON SCHEMA identity TO {role}; GRANT SELECT, INSERT, UPDATE, DELETE ON identity.oauth_token, identity.oauth_authorization TO {role};")
        .expect("writing to a String cannot fail");
    sql
}

/// Non-secret connection functions available to the control host.
pub const CONTROL_CONNECTION_FUNCTIONS: &[&str] = &[
    "retired_connection_credential(text)",
    "connection_projection(text,text,text)",
    "publish_integration(text,integer,text,jsonb,text,text[],text[])",
    "configure_connection(text,text,jsonb)",
    "configure_external_system(text,text,jsonb)",
    "configure_integration(text,text,jsonb)",
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
pub const CONTROL_SURFACE: [(&str, &str, &[&str]); 10] = [
    ("identity", "oauth_authorization", CONTROL_WRITE),
    ("identity", "oauth_token", CONTROL_WRITE),
    ("identity", "org_memberships", CONTROL_WRITE),
    ("identity", "org_roles", CONTROL_WRITE),
    ("identity", "password_logins", &["SELECT"]),
    ("identity", "principals", &["SELECT"]),
    ("identity", "project_env_memberships", CONTROL_WRITE),
    ("identity", "project_roles", CONTROL_WRITE),
    ("registry", "project_envs", &["SELECT"]),
    ("registry", "projects", &["SELECT", "INSERT"]),
];

/// The columns the control host writes in a relation of which it otherwise only
/// reads, with the privilege it holds on them. The status routes write the
/// status of an environment (docs/architecture/execution.md#host-run-routes,
/// wamn-zua8.2). The other columns of `registry.project_envs` belong to
/// provisioning.
pub const CONTROL_COLUMN_SURFACE: [(&str, &str, &str, &[&str]); 1] =
    [("registry", "project_envs", "UPDATE", &["status"])];

/// The schemas of [`CONTROL_SURFACE`], in which the control role holds `USAGE`.
pub const CONTROL_SCHEMAS: [&str; 2] = ["identity", "registry"];

/// Converge the stable control role to exactly [`CONTROL_SURFACE`] and
/// [`CONTROL_COLUMN_SURFACE`] in the
/// control database. The role is not a [`PLATFORM_GROUP_ROLE`] member and
/// holds nothing else.
pub fn grant_control_surface_sql() -> String {
    format!(
        "{} {}",
        ensure_workload_acl_role_sql(WorkloadRoleFamily::Control),
        control_surface_grants_sql()
    )
}

/// The grants of [`grant_control_surface_sql`] without the role bootstrap.
///
/// The control owner can apply these grants without permission to create roles.
pub fn control_surface_grants_sql() -> String {
    let role = escape_identifier(WorkloadRoleFamily::Control.acl_role());
    let plane = SYSTEM_PLANE_SCHEMAS
        .iter()
        .map(|name| escape_identifier(name))
        .collect::<Vec<_>>()
        .join(", ");
    let schemas = CONTROL_SCHEMAS
        .iter()
        .map(|name| escape_identifier(name))
        .collect::<Vec<_>>()
        .join(", ");
    let mut sql = format!(
        "REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA {plane} FROM {role}; \
         REVOKE ALL PRIVILEGES ON SCHEMA {plane} FROM {role}; \
         GRANT USAGE ON SCHEMA {schemas} TO {role};"
    );
    for (schema, relation, privileges) in CONTROL_SURFACE {
        write!(
            sql,
            " GRANT {privileges} ON TABLE {schema}.{relation} TO {role};",
            privileges = privileges.join(", "),
            schema = escape_identifier(schema),
            relation = escape_identifier(relation),
        )
        .expect("writing to a String cannot fail");
    }
    for (schema, relation, privilege, columns) in CONTROL_COLUMN_SURFACE {
        write!(
            sql,
            " GRANT {privilege} ({columns}) ON TABLE {schema}.{relation} TO {role};",
            columns = columns
                .iter()
                .map(|column| escape_identifier(column))
                .collect::<Vec<_>>()
                .join(", "),
            schema = escape_identifier(schema),
            relation = escape_identifier(relation),
        )
        .expect("writing to a String cannot fail");
    }
    for signature in CONTROL_CONNECTION_FUNCTIONS {
        write!(sql, " DO $connection_acl$ BEGIN IF to_regprocedure('registry.{signature}') IS NOT NULL THEN GRANT EXECUTE ON FUNCTION registry.{signature} TO {role}; END IF; END $connection_acl$;").expect("writing to a String cannot fail");
    }
    sql
}

/// The family-specific privilege batch that converges one family's STABLE ACL
/// role to its exact grant set, when this crate owns that convergence.
///
/// `None` = the family's stable role carries no grant set this batch applies.
/// An admitted family lands on `None` with no edit here; a family
/// that acquires a grant set adds it beside its builder, which is the one place
/// a family stays per-family (`wamn-0h0g.22.16`).
pub fn stable_surface_sql(family: WorkloadRoleFamily) -> Option<String> {
    match family {
        WorkloadRoleFamily::ExecutorPlatform => {
            Some(grant_executor_platform_surface_sql("wamn_run"))
        }
        WorkloadRoleFamily::HttpAdmitter => Some(grant_http_admitter_surface_sql("wamn_run")),
        WorkloadRoleFamily::SessionRoleReader => Some(grant_session_role_reader_surface_sql()),
        WorkloadRoleFamily::RegistryReader => Some(grant_registry_reader_surface_sql()),
        WorkloadRoleFamily::IdentityReader => Some(grant_identity_reader_surface_sql()),
        WorkloadRoleFamily::ConnectionReader => Some(grant_connection_reader_surface_sql()),
        WorkloadRoleFamily::EventMaterializer => {
            Some(grant_event_materializer_surface_sql("wamn_run"))
        }
        WorkloadRoleFamily::Administration => Some(grant_administration_surface_sql("wamn_run")),
        WorkloadRoleFamily::Control => Some(grant_control_surface_sql()),
        _ => None,
    }
}

/// Record the owner-maintained login-identity-to-tenant mapping for one scope.
///
/// `$1` login identity, `$2` `env_key`, `$3` org, `$4` project, `$5` environment.
/// Tenant authority is database-authoritative: this row, not a caller-set
/// `app.env_key`, is what every author-accessed relation's restrictive policy
/// resolves through, so the statement is deliberately an owner statement with no
/// grant to the author. `DO UPDATE` is restricted to a row whose tenant already
/// matches, so a replay converges while a re-point to a different tenant returns
/// **zero rows** instead of widening one login's reach — the caller must read an
/// empty result as a refusal, not as a no-op.
pub fn upsert_control_author_env_key_mapping_sql() -> &'static str {
    "INSERT INTO wamn_authority.author_login_tenants \
       (login_identity, env_key, org_id, project_id, environment) \
     VALUES ($1, $2, $3, $4, $5) \
     ON CONFLICT (login_identity) DO UPDATE \
       SET org_id = EXCLUDED.org_id, \
           project_id = EXCLUDED.project_id, \
           environment = EXCLUDED.environment \
       WHERE author_login_tenants.env_key = EXCLUDED.env_key \
     RETURNING env_key"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The platform extension list is PINNED, because a package cannot install
    /// one and a silent removal would take an author's constraint with it.
    #[test]
    fn the_platform_installs_btree_gist_and_pg_stat_statements_only() {
        assert_eq!(PLATFORM_EXTENSIONS, ["btree_gist", "pg_stat_statements"]);
        assert_eq!(
            install_platform_extensions_sql(),
            "CREATE EXTENSION IF NOT EXISTS \"btree_gist\";\n\
             CREATE EXTENSION IF NOT EXISTS \"pg_stat_statements\";\n"
        );
    }

    #[test]
    fn ensure_app_role_is_a_passwordless_nologin_acl_role() {
        let sql = ensure_app_acl_role_sql();
        assert!(sql.contains("pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'))"));
        assert!(sql.contains("'wamn_app'"));
        assert!(sql.contains("CREATE ROLE \"wamn_app\" NOLOGIN"));
        assert!(sql.contains("ALTER ROLE \"wamn_app\" NOLOGIN PASSWORD NULL"));
        assert!(sql.contains("rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole"));
        assert!(sql.contains("FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app'"));
        assert!(
            !sql.contains("pg_authid"),
            "a non-superuser cannot read pg_authid"
        );
        // NOSUPERUSER only for a role that is a superuser (platform-ui §4.10).
        assert!(sql.contains(
            "IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app' AND rolsuper) \
             THEN EXECUTE format('ALTER ROLE %I NOSUPERUSER', 'wamn_app');"
        ));
        assert!(!sql.contains("PASSWORD NULL NOSUPERUSER"));
        assert!(sql.contains("EXCEPTION WHEN duplicate_object THEN"));
        assert!(!sql.contains("pg_terminate_backend"));
        for attr in [
            "NOSUPERUSER",
            "NOCREATEDB",
            "NOCREATEROLE",
            "NOINHERIT",
            "NOREPLICATION",
            "NOBYPASSRLS",
        ] {
            assert!(sql.contains(attr), "missing {attr}");
        }
    }

    #[test]
    fn app_role_drain_is_native_bounded_and_residue_refusing() {
        let sql = drain_app_role_sessions_sql();
        assert!(sql.contains("pg_catalog.pg_terminate_backend(stale_pid, 5000)"));
        assert!(sql.contains("pg_catalog.pg_stat_clear_snapshot()"));
        assert!(sql.contains("usename = 'wamn_app' AND pid <> pg_backend_pid()"));
        assert!(sql.contains("MESSAGE = 'wamn-app-session-drain-incomplete'"));
        assert_eq!(sql.matches("pg_stat_activity").count(), 2);
    }

    #[test]
    fn named_database_ddl_targets_an_arbitrary_db_name() {
        // The per-project-env path (wamn-q3n.7/.8) passes a full triple-derived name.
        assert_eq!(
            create_database_named_sql("wamn-db-acme--billing--dev"),
            "CREATE DATABASE \"wamn-db-acme--billing--dev\""
        );
        assert_eq!(
            drop_database_named_sql("wamn-db-acme--billing--dev"),
            "DROP DATABASE IF EXISTS \"wamn-db-acme--billing--dev\" WITH (FORCE)"
        );
    }

    /// The registry reader's WHOLE batch, pinned as the string the builder
    /// emits (`wamn-0h0g.12.116`).
    ///
    /// Generated text, not declared DDL: the assertion is over what this
    /// function produces, so a comment mentioning a GRANT cannot satisfy it and
    /// an added grant cannot hide inside it.
    #[test]
    fn the_registry_reader_grants_exactly_one_select_and_revokes_the_whole_plane() {
        let sql = grant_registry_reader_surface_sql();
        assert!(sql.starts_with(&ensure_workload_acl_role_sql(
            WorkloadRoleFamily::RegistryReader
        )));
        let granted = sql
            .split_once("END $workload_acl$;")
            .expect("the batch opens with the ACL-role guard")
            .1;
        assert_eq!(
            granted,
            " REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA \"identity\", \"provisioning\", \
             \"registry\" FROM \"wamn_registry_reader\"; \
             REVOKE ALL PRIVILEGES ON SCHEMA \"identity\", \"provisioning\", \"registry\" \
             FROM \"wamn_registry_reader\"; \
             GRANT USAGE ON SCHEMA \"registry\" TO \"wamn_registry_reader\"; \
             GRANT SELECT ON TABLE \"registry\".\"event_readers\" TO \"wamn_registry_reader\"; \
             GRANT SELECT ON TABLE \"registry\".\"capture_gap\" TO \"wamn_registry_reader\";"
        );
    }

    /// THE DISJOINTNESS GUARD, from the registry side (`wamn-0h0g.12.116`).
    ///
    /// Two SELECTs on two registry relations, and not one syllable of `identity`
    /// downstream of the revocation. Widening this role onto the identity plane
    /// — the one edit that would collapse the two disjoint grant sets into a
    /// union — fails here.
    #[test]
    fn the_registry_reader_is_never_granted_anything_on_the_identity_plane() {
        let sql = grant_registry_reader_surface_sql();
        assert_eq!(sql.matches("GRANT SELECT").count(), 2);
        assert_eq!(sql.matches("GRANT USAGE ON SCHEMA").count(), 1);
        assert_eq!(REGISTRY_READER_RELATIONS.len(), 2);
        for forbidden in ["INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES"] {
            assert!(
                !sql.contains(&format!("GRANT {forbidden}")),
                "the registry reader must hold no {forbidden}"
            );
        }
        let granted = sql
            .rsplit_once("FROM \"wamn_registry_reader\";")
            .expect("the batch revokes before it grants")
            .1;
        for plane in ["identity", "provisioning"] {
            assert!(
                !granted.contains(plane),
                "the registry reader is granted something on the {plane} plane"
            );
        }
        // The revocation half, conversely, MUST name every system-plane schema:
        // that is what makes a role widened out of band converge back.
        for plane in SYSTEM_PLANE_SCHEMAS {
            assert!(
                sql.contains(&format!("\"{plane}\"")),
                "the batch does not revoke across the {plane} plane"
            );
        }
    }

    /// The registry reader's grant set reaches the generic lifecycle, so a
    /// prepared generation inherits exactly it and nothing wider.
    #[test]
    fn the_registry_reader_prepare_carries_its_grant_set_and_only_control_connect() {
        let role = "wamn_registry_reader_c8216ab3ed30b424606deacec7d0d7cb7e65649b_a";
        let sql = prepare_workload_generation_sql(
            WorkloadRoleFamily::RegistryReader,
            "wamn_system",
            role,
            "secret",
            "2026-09-15T12:00:00Z",
        );
        assert!(sql.contains(&grant_registry_reader_surface_sql()));
        assert!(sql.contains(&format!(
            "GRANT \"wamn_registry_reader\" TO \"{role}\" \
             WITH ADMIN FALSE, INHERIT TRUE, SET FALSE"
        )));
        assert_eq!(sql.matches("GRANT CONNECT ON DATABASE").count(), 1);
        assert!(sql.contains("GRANT CONNECT ON DATABASE \"wamn_system\""));
        assert!(stable_surface_sql(WorkloadRoleFamily::RegistryReader).is_some());
    }

    /// The identity reader's WHOLE batch, pinned as the string the builder
    /// emits (`wamn-0h0g.12.67`).
    #[test]
    fn the_identity_reader_grants_exactly_four_selects_and_revokes_the_whole_plane() {
        let sql = grant_identity_reader_surface_sql();
        assert!(sql.starts_with(&ensure_workload_acl_role_sql(
            WorkloadRoleFamily::IdentityReader
        )));
        let granted = sql
            .split_once("END $workload_acl$;")
            .expect("the batch opens with the ACL-role guard")
            .1;
        assert_eq!(
            granted,
            " REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA \"identity\", \"provisioning\", \
             \"registry\" FROM \"wamn_identity_reader\"; \
             REVOKE ALL PRIVILEGES ON SCHEMA \"identity\", \"provisioning\", \"registry\" \
             FROM \"wamn_identity_reader\"; \
             GRANT USAGE ON SCHEMA \"identity\" TO \"wamn_identity_reader\"; \
             GRANT SELECT ON TABLE \"identity\".\"pats\" TO \"wamn_identity_reader\"; \
             GRANT SELECT ON TABLE \"identity\".\"principals\" TO \"wamn_identity_reader\"; \
             GRANT SELECT ON TABLE \"identity\".\"project_env_memberships\" TO \"wamn_identity_reader\"; \
             GRANT SELECT ON TABLE \"identity\".\"project_roles\" TO \"wamn_identity_reader\"; \
             GRANT SELECT ON TABLE \"identity\".\"password_logins\" TO \"wamn_identity_reader\"; \
             GRANT SELECT (principal_id, org, status) ON identity.org_memberships TO wamn_identity_reader;"
        );
    }

    /// THE THREE-TIMES-DRIFT GUARD (`wamn-0h0g.12.67`).
    ///
    /// The live cluster role carries `SELECT, INSERT, UPDATE`. That `INSERT` was
    /// added deliberately when `POST /login` existed, and the grant drifted
    /// SELECT-only -> +INSERT -> +INSERT,UPDATE out of band three separate
    /// times. Whoever holds `INSERT` on `identity.pats` can bind a token digest
    /// to any principal and present the matching PAT; whoever holds it on
    /// `identity.project_roles` can self-grant a role in any project — and
    /// `identity.*` has no row-level security to confine either. This is the
    /// assertion that fails on the fourth drift instead of after it.
    #[test]
    fn the_identity_reader_grants_no_insert_and_no_update() {
        let sql = grant_identity_reader_surface_sql();
        for forbidden in ["INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES"] {
            assert!(
                !sql.contains(&format!("GRANT {forbidden}")),
                "the identity reader must hold no {forbidden} — see this test's doc comment"
            );
        }
        assert_eq!(
            sql.matches("GRANT SELECT").count(),
            IDENTITY_READER_RELATIONS.len() + 1,
        );
        // The other half of the premise — that neither consuming statement takes
        // a row lock, which would need UPDATE that SELECT alone cannot serve —
        // is pinned where those statements live, by
        // `wamn_platform_identity`'s `the_two_reads_the_management_surface_makes_take_no_row_lock`.
    }

    /// THE DISJOINTNESS GUARD, from both sides (`wamn-0h0g.12.116`,
    /// `wamn-0h0g.12.67`).
    ///
    /// One role serving both grant sets is how a credential gets widened to the
    /// union, so the two batches must name two roles and neither may grant
    /// anything inside the other's schema.
    #[test]
    fn the_two_control_reader_grant_sets_are_disjoint() {
        let registry = grant_registry_reader_surface_sql();
        let identity = grant_identity_reader_surface_sql();
        assert!(!registry.contains("wamn_identity_reader"));
        assert!(!identity.contains("wamn_registry_reader"));
        for (batch, own, foreign) in [
            (&registry, "registry", "identity"),
            (&identity, "identity", "registry"),
        ] {
            let granted = batch
                .split_once(&format!("GRANT USAGE ON SCHEMA \"{own}\""))
                .expect("the batch revokes before it grants")
                .1;
            assert!(
                !granted.contains(foreign),
                "a control reader is granted something on the {foreign} plane"
            );
            // The revocation half, conversely, MUST span the whole plane: that
            // is what converges a role widened out of band back to its own.
            for plane in SYSTEM_PLANE_SCHEMAS {
                assert!(batch.contains(&format!("\"{plane}\"")));
            }
        }
        assert!(
            IDENTITY_READER_RELATIONS
                .iter()
                .all(|relation| !REGISTRY_READER_RELATIONS.contains(relation))
        );
    }

    /// The identity reader's grant set reaches the generic lifecycle, so a
    /// prepared generation inherits exactly it and nothing wider.
    #[test]
    fn the_identity_reader_prepare_carries_its_grant_set_and_only_control_connect() {
        let role = "wamn_identity_reader_4ea81a9f35b250844e8e95f9985cf1e4f7c16dba_a";
        let sql = prepare_workload_generation_sql(
            WorkloadRoleFamily::IdentityReader,
            "wamn_system",
            role,
            "secret",
            "2026-09-15T12:00:00Z",
        );
        assert!(sql.contains(&grant_identity_reader_surface_sql()));
        assert!(sql.contains(&format!(
            "GRANT \"wamn_identity_reader\" TO \"{role}\" \
             WITH ADMIN FALSE, INHERIT TRUE, SET FALSE"
        )));
        assert_eq!(sql.matches("GRANT CONNECT ON DATABASE").count(), 1);
        assert!(sql.contains("GRANT CONNECT ON DATABASE \"wamn_system\""));
        assert!(stable_surface_sql(WorkloadRoleFamily::IdentityReader).is_some());
    }
    #[test]
    fn generation_state_probe_is_read_only_and_parameterized() {
        let sql = workload_generation_state_sql();
        assert!(sql.starts_with("SELECT"));
        assert!(sql.contains("rolcanlogin"));
        assert!(sql.contains("FROM pg_catalog.pg_roles r"));
        assert!(!sql.contains("rolpassword IS NOT NULL AS"));
        assert!(sql.contains("pg_auth_members"));
        assert!(sql.contains("NOT m.admin_option AND m.inherit_option AND NOT m.set_option"));
        assert!(sql.contains("AS generation_children_exact"));
        assert!(sql.contains("child_edge.roleid = r.oid"));
        assert!(sql.contains("parent_edge.member = generation.oid) <> 1"));
        assert!(sql.contains("direct_acl.privilege_type <> 'CONNECT'"));
        assert!(!sql.contains(") > 2"));
        assert!(!sql.contains("count(DISTINCT substring"));
        assert!(sql.contains("WHERE m.roleid = r.oid"));
        assert!(sql.contains("rolvaliduntil"));
        assert!(sql.contains("isfinite"));
        assert!(sql.contains("aclexplode"));
        assert!(sql.contains("pg_stat_activity"));
        assert!(sql.contains("r.rolname = $1"));
        for mutation in ["ALTER ", "GRANT ", "REVOKE ", "UPDATE ", "DELETE "] {
            assert!(!sql.contains(mutation));
        }
    }

    #[test]
    fn generation_grants_cover_cluster_and_database_object_classes() {
        let sql = role_database_grants_sql();
        for catalog in [
            "pg_database",
            "pg_namespace",
            "pg_class",
            "pg_proc",
            "pg_attribute",
            "pg_type",
            "pg_language",
            "pg_largeobject_metadata",
            "pg_foreign_data_wrapper",
            "pg_foreign_server",
            "pg_tablespace",
            "pg_parameter_acl",
            "pg_default_acl",
        ] {
            assert!(sql.contains(catalog), "missing direct ACL class {catalog}");
        }
        assert!(sql.contains("x.grantee = (SELECT oid FROM wanted)"));
    }

    #[test]
    fn generation_cluster_probes_are_read_only_and_scope_locked() {
        assert!(public_connect_databases_sql().starts_with("SELECT"));
        assert!(public_connect_databases_sql().contains("acl.grantee = 0"));
        assert!(public_connect_databases_sql().contains("privilege_type = 'CONNECT'"));
        assert!(!public_connect_databases_sql().contains("TEMPORARY"));
        assert!(revoke_public_connect_floor_sql().starts_with("DO $$"));
        // The floor is CONNECTABILITY-filtered, never template-filtered:
        // `template1` is a template AND connectable, and a template filter left
        // PostgreSQL's default PUBLIC CONNECT on it. `template0`
        // (`datallowconn = false`) stays out of scope either way.
        assert!(revoke_public_connect_floor_sql().contains("WHERE d.datallowconn"));
        assert!(!revoke_public_connect_floor_sql().contains("datistemplate"));
        assert!(public_connect_databases_sql().contains("WHERE d.datallowconn"));
        assert!(!public_connect_databases_sql().contains("datistemplate"));
        assert!(revoke_public_connect_floor_sql().contains("REVOKE CONNECT ON DATABASE %I"));
        assert!(!revoke_public_connect_floor_sql().contains("TEMPORARY"));
        assert!(public_temporary_on_current_database_sql().starts_with("SELECT EXISTS"));
        assert!(public_temporary_on_current_database_sql().contains("current_database()"));
        assert!(public_temporary_on_current_database_sql().contains("'TEMPORARY'"));
        assert!(non_template_databases_sql().contains("NOT datistemplate"));
        assert_eq!(
            workload_scope_lock_sql(),
            "SELECT pg_advisory_lock(hashtextextended($1::text, 0))"
        );
    }

    #[test]
    fn replication_role_is_replication_login_and_otherwise_least_privilege() {
        let sql = ensure_replication_role_sql("wamn_cdc_acme__billing__dev", "s3cr3t");
        assert!(sql.contains("IF NOT EXISTS"), "idempotent guard");
        assert!(sql.contains("CREATE ROLE \"wamn_cdc_acme__billing__dev\" LOGIN REPLICATION"));
        // An existing role gets the same password and attributes (wamn-fipl).
        assert!(sql.contains(
            "ELSE ALTER ROLE \"wamn_cdc_acme__billing__dev\" LOGIN REPLICATION PASSWORD 's3cr3t' \
             NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS;"
        ));
        assert!(sql.contains("AND rolsuper) THEN EXECUTE format('ALTER ROLE %I NOSUPERUSER'"));
        assert!(sql.contains("PASSWORD 's3cr3t'"));
        // The R8b tier: REPLICATION but nothing else elevated. NOINHERIT is the
        // house default every other minted role carries.
        for attr in [
            "NOSUPERUSER",
            "NOCREATEDB",
            "NOCREATEROLE",
            "NOINHERIT",
            "NOBYPASSRLS",
        ] {
            assert!(sql.contains(attr), "missing {attr}");
        }
        // A password with a quote is escaped, not injected.
        assert!(ensure_replication_role_sql("r", "a'b").contains("PASSWORD 'a''b'"));
    }

    #[test]
    fn publication_covers_the_schema_and_is_idempotent() {
        let sql = create_publication_sql("wamn_cdc_acme__billing__dev", "app");
        // FOR TABLES IN SCHEMA (auto-includes tables created later) — never the
        // per-table form and never FOR ALL TABLES (which would leak platform
        // and any other schema into the stream).
        assert!(sql.contains(
            "CREATE PUBLICATION \"wamn_cdc_acme__billing__dev\" FOR TABLES IN SCHEMA \"app\""
        ));
        assert!(!sql.contains("FOR ALL TABLES"));
        // TRUNCATE is excluded at the source: the publish list names only the
        // three row operations the event plane carries (wamn-0h0g.19.19).
        assert!(sql.contains("WITH (publish = 'insert, update, delete')"));
        assert!(!sql.contains("truncate"));
        // Idempotent: guarded by a pg_publication probe (no IF NOT EXISTS in PG).
        assert!(sql.contains("IF NOT EXISTS (SELECT FROM pg_publication WHERE pubname = 'wamn_cdc_acme__billing__dev')"));
        // The eager schema guard is a separate statement.
        assert_eq!(
            ensure_schema_sql("app"),
            "CREATE SCHEMA IF NOT EXISTS \"app\""
        );
    }

    #[test]
    fn failover_slot_uses_the_sql_function_form_with_failover_true() {
        let sql = create_failover_slot_sql("wamn_cdc_acme__billing__dev");
        // The PG17+ five-argument form: (slot, plugin, temporary, twophase,
        // FAILOVER) — pgoutput, non-temporary, no two-phase, failover=true, the
        // exact shape pg_walstream's ensure_replication_slot tolerates.
        assert!(sql.contains(
            "pg_create_logical_replication_slot('wamn_cdc_acme__billing__dev', 'pgoutput', false, false, true)"
        ));
        // Idempotent: guarded by a pg_replication_slots probe.
        assert!(
            sql.contains("IF NOT EXISTS (SELECT FROM pg_replication_slots WHERE slot_name = 'wamn_cdc_acme__billing__dev')")
        );
    }

    /// The CDC role reads WAL, not application tables. Its only table reads are
    /// the two relation-classification maps, and the retroactive `REVOKE` must
    /// precede both narrow grants or it would strip them.
    #[test]
    fn replication_grants_cover_connect_usage_and_only_the_classification_maps() {
        let sql = grant_replication_access_sql("wamn-db-acme--billing--dev", "wamn_cdc_x", "app");
        assert!(sql.contains(
            "GRANT CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" TO \"wamn_cdc_x\""
        ));
        assert!(sql.contains("GRANT USAGE ON SCHEMA \"app\" TO \"wamn_cdc_x\""));
        assert!(sql.contains("GRANT SELECT ON \"app\".wamn_entities TO \"wamn_cdc_x\""));
        assert!(sql.contains("GRANT SELECT ON \"app\".wamn_cdc_exclusions TO \"wamn_cdc_x\""));
        // Never the blanket read of every tenant table.
        assert!(!sql.contains("GRANT SELECT ON ALL TABLES"));
        // The retroactive half, ordered before the narrow grant it would strip.
        let revoke = sql
            .find("REVOKE SELECT ON ALL TABLES IN SCHEMA \"app\" FROM \"wamn_cdc_x\"")
            .expect("retroactive revoke for already-provisioned environments");
        let grant = sql
            .find("GRANT SELECT ON \"app\".wamn_entities")
            .expect("entity-map grant");
        let exclusion_grant = sql
            .find("GRANT SELECT ON \"app\".wamn_cdc_exclusions")
            .expect("exclusion-map grant");
        assert!(
            revoke < grant,
            "the revoke would strip the entity-map grant"
        );
        assert!(
            revoke < exclusion_grant,
            "the revoke would strip the exclusion-map grant"
        );
    }

    /// R9: the database owner is a NOLOGIN title holder — never `wamn_app`
    /// (guest-authored SQL runs as it) and never a superuser.
    #[test]
    fn db_owner_role_is_nologin_title_only_and_hardens_on_replay() {
        let sql = ensure_db_owner_role_sql();
        assert!(sql.contains(DB_OWNER_ROLE));
        assert!(sql.contains("pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'))"));
        assert!(sql.contains(
            "CREATE ROLE wamn_db_owner NOLOGIN NOSUPERUSER NOCREATEDB \
             NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS"
        ));
        // A replay that finds a drifted attribute repairs it, not succeeds.
        assert!(sql.contains("ELSIF EXISTS"));
        assert!(sql.contains(
            "ALTER ROLE wamn_db_owner NOLOGIN NOCREATEDB \
             NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS"
        ));
        assert!(sql.contains("AND rolsuper) THEN ALTER ROLE wamn_db_owner NOSUPERUSER;"));
        // Owner-only by construction: no grants, no memberships, no generation
        // pair — a role nobody logs in as has nothing to rotate.
        for forbidden in ["GRANT ", "PASSWORD", "VALID UNTIL", "LOGIN PASSWORD"] {
            assert!(!sql.contains(forbidden), "db owner grew {forbidden}");
        }
        assert!(!sql.contains(APP_ROLE), "db owner is not wamn_app");
        assert!(!sql.contains("postgres"), "db owner is not a superuser");
    }

    /// The statement the operator runs by hand for a database created before
    /// its `Database` CR set `spec.owner`.
    #[test]
    fn database_owner_converges_to_the_title_role() {
        assert_eq!(
            set_database_owner_sql("wamn-db-acme--billing--dev"),
            "ALTER DATABASE \"wamn-db-acme--billing--dev\" OWNER TO \"wamn_db_owner\""
        );
        // Never re-points a database at the guest-reachable role or a superuser.
        let sql = set_database_owner_sql("wamn-db-acme--billing--dev");
        assert!(!sql.contains("wamn_app"));
        assert!(!sql.contains("postgres"));
    }

    #[test]
    fn cdc_teardown_builders_are_guarded() {
        assert_eq!(
            drop_publication_sql("wamn_cdc_x"),
            "DROP PUBLICATION IF EXISTS \"wamn_cdc_x\""
        );
        let drop_slot = drop_replication_slot_sql("wamn_cdc_x");
        assert!(drop_slot.contains("pg_drop_replication_slot('wamn_cdc_x')"));
        assert!(drop_slot.contains("IF EXISTS"));
    }
}

//! Pure Postgres text builders for project provisioning (SR3 house rule 3:
//! pure text + validated/quoted identifiers; the driver holds the connection).
//!
//! Every builder takes an **already-validated** name (see
//! [`crate::validate_project_env`]); a database name it takes is
//! double-quoted, so a slug (which cannot contain a `"`) is injection-safe. The
//! `wamn_app` role name is a pinned constant. Values that vary (a probed role
//! name, a role password) travel as `$n` params or quoted literals.

use crate::project_key::{PROJECT_KEY_FUNCTION, PROJECT_KEY_SCHEMA};

mod cdc;
mod credentials;
mod database;
mod database_grants;

#[doc(inline)]
use std::fmt::Write as _;

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
    CDC_PUBLISH, PUBLICATION_PROBE_SQL, create_failover_slot_sql, create_publication_schemas_sql,
    create_publication_sql, drop_publication_sql, drop_replication_slot_sql,
    ensure_cdc_exclusion_map_sql, ensure_entity_map_sql, ensure_schema_sql,
    grant_replication_access_sql, upsert_cdc_exclusion_map_sql, upsert_entity_map_sql,
};

use crate::name::{APP_ROLE, DB_OWNER_ROLE};
use crate::workload_role::{
    ADMINISTRATION_ROLE, EVENT_MATERIALIZER_ROLE, EXECUTOR_PLATFORM_ROLE, HTTP_ADMITTER_ROLE,
    PLATFORM_GROUP_ROLE, SESSION_ROLE_READER_ROLE, WorkloadRoleFamily,
};
pub(crate) use postgres_protocol::escape::escape_identifier;
use postgres_protocol::escape::escape_literal;

/// Idempotently create or harden the shared, cluster-global [`APP_ROLE`] as a
/// stable NOLOGIN ACL role.
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
    ensure_acl_role_sql(APP_ROLE)
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
    "project_env_key",
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
            ("platform", "integration"),
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
///   `result_json`, the caller-outcome family, `updated_at`
/// * `transitions::release_caller_sql` — the caller-outcome family, `updated_at`
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
pub const EXECUTOR_PLATFORM_RUN_INSERT_COLUMNS: [&str; 21] = [
    "project_env_key",
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
pub const EXECUTOR_PLATFORM_QUEUE_INSERT_COLUMNS: [&str; 2] = ["project_env_key", "run_id"];
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

/// `GRANT EXECUTE` on the project-key derivation for one stable ACL role.
///
/// # A WRITER needs this, not only a reader of the predicate
///
/// Every governed relation carries a `<table>_tkey` EXPRESSION INDEX over
/// `wamn_authority.project_key(text)`, and PostgreSQL evaluates that expression
/// while inserting or updating the row. So a family holding a COMPLETE table
/// grant set and the `wamn_platform` base policy arm still fails its INSERT with
/// 42501 `permission denied for function project_key` unless it holds this
/// (`wamn-0h0g.22.28`). Measured on PostgreSQL 18.6 as a login asserted NOT
/// (`rolsuper` OR `rolbypassrls`): with the grant the row lands, without it the
/// statement raises 42501 and the post-state is zero rows.
///
/// # Exactly this, and nothing beside it
///
/// * NOT `USAGE` on `wamn_authority`. An index expression is a stored, already
///   resolved node tree, so no name resolution happens at insert time and only
///   the function's EXECUTE ACL is validated. Measured: EXECUTE alone, with
///   `has_schema_privilege(role, 'wamn_authority', 'USAGE') = false`, inserts.
/// * NOT `current_project_key()`. That derivation is called only by the project
///   base policy, which `wamn-0h0g.22.17` narrowed `TO wamn_app`; a non-guest
///   family matches the permissive `TO wamn_platform` arm, whose `USING (true)`
///   calls nothing. Measured: the INSERT above succeeds with
///   `has_function_privilege(role, 'wamn_authority.current_project_key()',
///   'EXECUTE') = false`.
/// * NOT to [`PLATFORM_GROUP_ROLE`]. That group carries POLICY MEMBERSHIP only
///   and holds no privilege of its own; granting function EXECUTE to it would
///   hand the derivation to every member, including the families that never
///   write. It is granted here, beside the one family's own grant set, so the
///   surface stays the measured production surface.
fn grant_project_key_execute_sql(role: &str) -> String {
    format!(
        "GRANT EXECUTE ON FUNCTION {schema}.project_key(text) TO {role};",
        schema = escape_identifier(crate::project_key::PROJECT_KEY_SCHEMA),
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
///   admit, lease or terminalize a run. `schema` is therefore the REVOKE
///   target and never a grant target — an environment where someone handed this
///   family a run-plane privilege converges back on the next apply.
/// * NO write of any grain, so no `wamn_authority.project_key(text)` EXECUTE
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
/// It holds nothing on the run plane. `schema` is the run-plane revoke target.
pub fn grant_administration_surface_sql(schema: &str) -> String {
    let role = escape_identifier(ADMINISTRATION_ROLE);
    let schema = escape_identifier(schema);
    let mut sql = format!(
        "{ensure} \
         REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, platform, {schema} FROM {role}; \
         REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, platform, wamn_history, {schema} \
         FROM {role}; \
         REVOKE ALL PRIVILEGES ON SCHEMA catalog, platform, wamn_history, {schema} FROM {role}; \
         REVOKE EXECUTE ON FUNCTION {PROJECT_KEY_FUNCTION}(text) FROM {role}; \
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
         REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, platform, wamn_run, {PROJECT_KEY_SCHEMA} FROM {role}; \
         REVOKE ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA catalog, platform, wamn_run, {PROJECT_KEY_SCHEMA} FROM {role}; \
         REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, platform, wamn_run, {PROJECT_KEY_SCHEMA} FROM {role}; \
         DO $session_column_acl$ DECLARE target record; BEGIN \
           FOR target IN \
             SELECT n.nspname, c.relname, a.attname \
               FROM pg_catalog.pg_attribute a \
               JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
               JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
              WHERE n.nspname IN ('catalog', 'platform', 'wamn_run', '{PROJECT_KEY_SCHEMA}') \
                AND c.relkind IN ('r', 'p', 'v', 'm', 'f') \
                AND a.attnum > 0 AND NOT a.attisdropped AND a.attacl IS NOT NULL \
           LOOP \
             EXECUTE format('REVOKE ALL PRIVILEGES (%I) ON TABLE %I.%I FROM %I', \
               target.attname, target.nspname, target.relname, {role_literal}); \
           END LOOP; \
         END $session_column_acl$; \
         REVOKE ALL PRIVILEGES ON SCHEMA catalog, platform, wamn_run, {PROJECT_KEY_SCHEMA} FROM {role}; \
         GRANT USAGE ON SCHEMA platform TO {role}; \
         GRANT SELECT (id, status) ON TABLE platform.\"user\" TO {role}; \
         GRANT SELECT (user_id, role_name) ON TABLE platform.user_role TO {role};",
        ensure = ensure_workload_acl_role_sql(WorkloadRoleFamily::SessionRoleReader),
        role_literal = escape_literal(SESSION_ROLE_READER_ROLE),
    )
}

/// Converge the stable executor-platform role to its exact lease surface
/// (`wamn-0h0g.22.37`).
///
/// The production queries that select `AuthorityClass::ExecutorPlatform` live
/// in `wamn_runtime::plugins::wamn_postgres::production_lease` and its
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
/// * `wamn_authority.project_key(text)`: MEASURED, and not optional. `runs`
///   carries the `runs_tkey` EXPRESSION INDEX over that function, and
///   PostgreSQL evaluates an index expression while forming the new index entry
///   an UPDATE requires. Four of the six `runs` writes change `status`, which
///   participates in the partial-index predicates on `runs`, so those updates
///   can never take the HOT path that would skip index maintenance: the
///   evaluation — and therefore the EXECUTE test — is unconditional for them,
///   exactly as it is for the management admitter's INSERT (`wamn-0h0g.22.28`).
///   Schema `USAGE` on `wamn_authority` is NOT granted and is NOT needed: an
///   index expression is a stored, already-resolved node tree, so no name
///   resolution happens and only the function's EXECUTE ACL is validated.
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
    sql.push_str(&grant_project_key_execute_sql(EXECUTOR_PLATFORM_ROLE));
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
         REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, platform, {PROJECT_KEY_SCHEMA}, {schema} FROM {role}; \
         REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, platform, {PROJECT_KEY_SCHEMA}, {schema} FROM {role}; \
         REVOKE ALL PRIVILEGES ON SCHEMA catalog, platform, {PROJECT_KEY_SCHEMA}, {schema} FROM {role}; \
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

/// The two relations the CDC reader's `SELECT`s touch.
///
/// `services/cdc-reader` runs `wamn_registry::sql::select_event_reader_sql`
/// and `select_capture_gap_sql` over `WAMN_SYSTEM_URL` and nothing else, so this
/// is its whole authority (`wamn-59z6`: the reader reads its gap row).
pub const REGISTRY_READER_RELATIONS: [&str; 2] = ["event_readers", "capture_gap"];

/// The relations the management and user route identity reads touch.
///
/// `SELECT_PAT_BY_PREFIX_SQL` joins `pats` to `principals`; `SELECT_PROJECT_ROLES_SQL`
/// reads `project_roles`; user routes require `project_env_memberships`
/// (all in `crates/identity/platform/src/lib.rs`).
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
    sql.push_str(" GRANT EXECUTE ON FUNCTION identity.is_project_admin(uuid,text,text,text), identity.has_project_access(uuid,text,text,text) TO wamn_identity_reader;");
    sql
}

/// Grant the host its environment readers and encrypted OAuth storage.
pub fn grant_connection_reader_surface_sql() -> String {
    let mut sql =
        grant_system_reader_surface_sql(WorkloadRoleFamily::ConnectionReader, "registry", &[]);
    let role = escape_identifier(WorkloadRoleFamily::ConnectionReader.acl_role());
    sql.push_str(
        &wamn_registry::sql::control_provision_src_sql_grant_connection_reader_surface_sql(&role),
    );
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
pub const CONTROL_SURFACE: [(&str, &str, &[&str]); 12] = [
    ("identity", "oauth_authorization", CONTROL_WRITE),
    ("identity", "oauth_token", CONTROL_WRITE),
    ("identity", "org_memberships", CONTROL_WRITE),
    ("identity", "org_roles", CONTROL_WRITE),
    ("identity", "environment_roles", CONTROL_WRITE),
    ("identity", "password_logins", &["SELECT"]),
    ("identity", "principals", &["SELECT"]),
    ("identity", "project_env_memberships", CONTROL_WRITE),
    ("identity", "project_roles", CONTROL_WRITE),
    ("registry", "environment_projects", &["SELECT"]),
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
        sql.push_str(
            &wamn_registry::sql::control_provision_src_sql_control_surface_grants_sql_2(
                &role, signature,
            ),
        );
    }
    sql.push_str(wamn_registry::sql::CONTROL_PROVISION_SRC_SQL_CONTROL_SURFACE_GRANTS_SQL);
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
        WorkloadRoleFamily::Query => Some(grant_dynamic_sql_surface_sql()),
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

/// Record the owner-maintained login-identity-to-project mapping for one target.
///
/// `$1` login identity, `$2` `project_env_key`, `$3` org, `$4` project, `$5` environment.
/// Project authority is database-authoritative: this row, not a caller-set
/// `app.project_env_key`, is what every author-accessed relation's restrictive policy
/// resolves through, so the statement is deliberately an owner statement with no
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

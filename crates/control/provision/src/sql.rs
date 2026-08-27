//! Pure Postgres text builders for project provisioning (SR3 house rule 3:
//! pure text + validated/quoted identifiers; the driver holds the connection).
//!
//! Every builder takes an **already-validated** project id (see
//! [`crate::validate_project_id`]); the database name it derives is
//! double-quoted, so a slug (which cannot contain a `"`) is injection-safe. The
//! `wamn_app` role name is a pinned constant. Values that vary (a probe's
//! database name, a role password) travel as `$n` params or quoted literals.

use crate::name::{APP_ROLE, DB_OWNER_ROLE, DISPATCH_READER_ROLE, database_name};
use crate::workload_role::{
    EXECUTOR_PLATFORM_ROLE, HTTP_ADMITTER_ROLE, MANAGEMENT_ADMITTER_ROLE, PLATFORM_GROUP_ROLE,
    WorkloadRoleFamily,
};
pub(crate) use wamn_pg_core::quote_ident;
use wamn_pg_core::quote_literal;

// Quote a SQL identifier (double-quoted, embedded `"` doubled). Mirrors the
// canonical `wamn_schema_compiler::sql::quote_ident` (inlined to keep this crate's
// dependency closure to `serde_json`).

// Quote a SQL string literal (single-quoted, embedded `'` doubled). Mirrors the
// canonical `wamn_schema_compiler::sql::quote_literal`.

/// Idempotently bootstrap the shared, cluster-global [`APP_ROLE`]. Runs in a
/// `DO` block so re-running against a cluster that already has the role is a
/// no-op. `NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS` — the role can only
/// do DML under RLS on tables explicitly granted to it (the S2/2.2 model). In
/// production the role is pre-created once; this makes the tool self-contained.
///
/// EXCEPTION-guarded under the shared `wamn_role_bootstrap` advisory lock
/// (`wamn-0h0g.12.186`), the [`ensure_platform_group_role_sql`] shape. Bare
/// `IF NOT EXISTS` is NOT idempotent under concurrency: roles are CLUSTER-global,
/// so two provisioners racing on a cluster where the role is absent both take
/// the `THEN` branch and the loser gets `duplicate_object`. The lock serializes
/// appliers that take it; the handler covers the ones that do not.
///
/// It deliberately keeps NO harden arm and re-stamps NO password on an existing
/// role: a replay must not overwrite a credential rotated out of band. Whether
/// this role should be `LOGIN` at all is `wamn-0h0g.12.140`'s cutover — after
/// it, every guest session is a per-tenant generation and
/// [`ensure_workload_acl_role_sql`] is what this builder becomes.
pub fn ensure_app_role_sql(password: &str) -> String {
    format!(
        "DO $app_role$ BEGIN \
           PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = {role_lit}) THEN \
             CREATE ROLE {role} LOGIN PASSWORD {pw} \
               NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
           END IF; \
         EXCEPTION WHEN duplicate_object THEN NULL; \
         END $app_role$;",
        role = quote_ident(APP_ROLE),
        role_lit = quote_literal(APP_ROLE),
        pw = quote_literal(password),
    )
}

/// Idempotently create or harden the database-owner role [`DB_OWNER_ROLE`] as
/// NOLOGIN, under the shared `wamn_role_bootstrap` advisory lock — the
/// [`ensure_control_author_acl_role_sql`] shape, so a replay that finds a
/// drifted attribute re-`ALTER`s it instead of reporting success.
///
/// **Zero grants, zero memberships, no generation pair.** The role exists only
/// to hold title; nothing connects as it, so it is deliberately outside the
/// credential-rotation machinery. Ownership is conferred declaratively by the
/// `Database` CR's `spec.owner`
/// ([`render_project_env_database`](crate::render_project_env_database)) and
/// convergently by [`set_database_owner_sql`].
///
/// **Apply this to the target cluster BEFORE any `Database` CR naming it** —
/// CNPG maps `spec.owner` to `CREATE DATABASE … OWNER` / `ALTER DATABASE …
/// OWNER TO`, and both fail against a role that does not exist yet.
pub fn ensure_db_owner_role_sql() -> &'static str {
    "DO $db_owner$ BEGIN \
       PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); \
       IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles \
                      WHERE rolname = 'wamn_db_owner') THEN \
         CREATE ROLE wamn_db_owner NOLOGIN NOSUPERUSER NOCREATEDB \
           NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
       ELSIF EXISTS (SELECT FROM pg_catalog.pg_roles \
                     WHERE rolname = 'wamn_db_owner' \
                       AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole \
                            OR rolinherit OR rolreplication OR rolbypassrls)) THEN \
         ALTER ROLE wamn_db_owner NOLOGIN NOSUPERUSER NOCREATEDB \
           NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
       END IF; \
     END $db_owner$;"
}

/// `ALTER DATABASE "<database>" OWNER TO "wamn_db_owner"` — the convergence
/// half of the ownership migration, for databases that already exist.
///
/// A `REVOKE` cannot express ownership, so moving an already-provisioned
/// project-env database off its old owner is this `ALTER`, not a grant edit.
/// It is naturally idempotent (setting the owner a database already has is a
/// no-op), so the privilege batch stays re-runnable and no one-shot migration
/// script is needed: fresh databases arrive owned correctly from the CR, old
/// ones converge here, and re-applying converges again.
///
/// **Order is load-bearing: this must run BEFORE
/// [`grant_connect_on_database_sql`].** `ALTER DATABASE … OWNER TO` rewrites the
/// outgoing owner's ACL entry to the incoming owner, and `wamn_app`'s granted
/// `CONNECT` merges into that entry while `wamn_app` is the owner — so an
/// owner change applied *after* the grant silently destroys it.
///
/// Run as the superuser provisioning principal (which needs no membership in
/// the new owner), connected to any database on the target cluster, AFTER the
/// database exists.
pub fn set_database_owner_sql(database: &str) -> String {
    format!(
        "ALTER DATABASE {db} OWNER TO {owner}",
        db = quote_ident(database),
        owner = quote_ident(DB_OWNER_ROLE),
    )
}

/// `CREATE DATABASE "<database>"`, naming the database directly (not derived from
/// a project id) — the per-project-env counterpart of [`create_database_sql`],
/// mirroring [`grant_connect_on_database_sql`] taking an already-derived name.
/// The name is double-quoted (a slug-derived name cannot contain a `"`, so it is
/// injection-safe). Must run as its own autocommit statement (Postgres forbids
/// `CREATE DATABASE` inside a transaction block).
///
/// Pass a name from [`project_env_database_name`](crate::project_env_database_name)
/// (`wamn-db-<org>--<project>--<env>--<instance>`). In production the CNPG `Database` CRD
/// creates the per-project-env database; this is the plain-SQL equivalent the
/// substrate-agnostic gate uses off-cluster (wamn-q3n.8).
pub fn create_database_named_sql(database: &str) -> String {
    format!("CREATE DATABASE {}", quote_ident(database))
}

/// `CREATE DATABASE "wamn-db-<project>" OWNER "wamn_db_owner"`. Must run as its
/// own autocommit statement (Postgres forbids `CREATE DATABASE` inside a
/// transaction block).
///
/// Unlike [`create_database_named_sql`], this legacy `provision-project`
/// wrapper assigns the stable NOLOGIN title role directly. The named helper
/// remains owner-neutral because its callers model substrate-created databases.
pub fn create_database_sql(project: &str) -> String {
    format!(
        "{} OWNER {}",
        create_database_named_sql(&database_name(project)),
        quote_ident(DB_OWNER_ROLE),
    )
}

/// `DROP DATABASE IF EXISTS "<database>" WITH (FORCE)`, naming the database
/// directly — the per-project-env counterpart of [`drop_database_sql`] (teardown /
/// gate only; destructive). Autocommit.
pub fn drop_database_named_sql(database: &str) -> String {
    format!(
        "DROP DATABASE IF EXISTS {} WITH (FORCE)",
        quote_ident(database)
    )
}

/// `DROP DATABASE IF EXISTS "wamn-db-<project>" WITH (FORCE)` — teardown / gate
/// only (destructive; the production tool never drops). Autocommit.
pub fn drop_database_sql(project: &str) -> String {
    drop_database_named_sql(&database_name(project))
}

/// Probe whether a database exists. The database **name** is the `$1`
/// parameter (a value, not an interpolated identifier); pass
/// [`database_name`]`(project)`.
pub fn database_exists_sql() -> &'static str {
    "SELECT EXISTS (SELECT FROM pg_database WHERE datname = $1)"
}

/// Restrict `CONNECT` on a database — named by its full, already-derived name —
/// to the shared app role: revoke `CONNECT` and `TEMPORARY` from `PUBLIC`
/// (PostgreSQL grants both on new databases by default) and grant `CONNECT` to
/// [`APP_ROLE`]. The name is
/// double-quoted (a slug-derived name cannot contain a `"`, so it is
/// injection-safe). Both statements are idempotent; issue as one batch.
///
/// This is the "thin imperative privilege step" the CNPG `Database` CRD does not
/// cover (per-project-env provisioning, wamn-q3n.7): the CRD creates the database
/// declaratively, but `REVOKE CONNECT FROM PUBLIC` / `GRANT` is run here.
///
/// # THE `GRANT` HALF CONFINES NOTHING, AND THE CLAIM THAT IT DID IS RETRACTED
///
/// This doc used to call the `GRANT` defense-in-depth behind the routing of a
/// component to exactly one project database. `wamn-0h0g.12.179` MEASURED that
/// layer on a certified-fresh PostgreSQL 18 and it does not confine at all:
/// [`APP_ROLE`] is CLUSTER-GLOBAL and every per-tenant generation is a member of
/// it `WITH INHERIT TRUE`, so a single `GRANT CONNECT` per environment lets a
/// generation minted for ONE project-env database open a real session on ANY
/// OTHER the batch has run against. `wamn-0h0g.22.24` measured the identical
/// shape for [`DISPATCH_READER_ROLE`]. CONNECT belongs to the GENERATION, which
/// [`prepare_workload_generation_sql`] grants it directly and only for its own
/// database.
///
/// The production per-project-env path therefore does NOT call this builder any
/// more: `provision_project_env::privilege_sql` REVOKES both stable roles
/// instead. What still reaches it is the LEGACY `provision-project` verb
/// (`services/ctl/src/provision.rs`, through [`grant_connect_sql`]) and a set of
/// live-gate fixtures that use it to give `wamn_app` a dialable session of their
/// own. Confining or deleting that path is `wamn-0h0g.12.185`; until it lands,
/// the `PUBLIC` revoke is the only half of this batch that confines anything.
pub fn grant_connect_on_database_sql(database: &str) -> String {
    let db = quote_ident(database);
    format!(
        "REVOKE CONNECT, TEMPORARY ON DATABASE {db} FROM PUBLIC; \
         GRANT CONNECT ON DATABASE {db} TO {role};",
        role = quote_ident(APP_ROLE),
    )
}

/// Restrict `CONNECT` on the per-project database `wamn-db-<project>` (2.3) — a
/// thin wrapper over [`grant_connect_on_database_sql`] with the project's derived
/// database name.
pub fn grant_connect_sql(project: &str) -> String {
    grant_connect_on_database_sql(&database_name(project))
}

// --- Dispatcher read-only provisioning (wamn-0h0g.12.66) ---------------------
//
// The always-on dispatcher authenticated as the shared, guest-reachable
// [`APP_ROLE`], which holds `INSERT`/`UPDATE`/`DELETE` on the run queue and on
// the whole `catalog` schema. Its real surface is two `SELECT`s. These builders
// mint the scoped reader and grant it exactly that surface, nothing wider.
//
// Deliberately parallel to the `CONNECT` builders above rather than folded into
// them: [`grant_connect_on_database_sql`] names [`APP_ROLE`] specifically, and a
// shared builder would be one edit away from handing the dispatcher's narrow
// credential the application role's authority, or vice versa.

/// The relations the dispatcher reads, in the order it touches them: the
/// parked-due reconciliation `SELECT` scans `run_queue` and its budget clause
/// `EXISTS`-joins `effect_attempts`; the queue-depth `SELECT` reads the same
/// pair. PostgreSQL checks privileges on every relation a statement references
/// regardless of whether the subquery yields rows, so BOTH grants are load
/// bearing even when the ledger is empty.
pub const DISPATCH_READER_RELATIONS: [&str; 2] = ["run_queue", "effect_attempts"];

/// Catalog relations read by the surviving management-admission statement.
///
/// ALSO the callable-HTTP admitter's WHOLE surface (`wamn-0h0g.22.37`). That is
/// a MEASURED coincidence, not a shared abstraction: the admitter's one
/// production statement,
/// `wamn_runtime::plugins::wamn_postgres::claims::CONNECTION_EFFECT_SNAPSHOT_SQL`,
/// names these six and no others. It is reused rather than copied because a
/// second array of the same six strings is a second thing to keep true, and the
/// two families' builders stay separate — which is where they actually differ.
pub const MANAGEMENT_ADMITTER_CATALOG_RELATIONS: [&str; 6] = [
    "wirings",
    "component_library",
    "connection_requirements",
    "connection_bindings",
    "connection_instances",
    "connection_generations",
];

/// Catalog relations the executor-platform family reads (`wamn-0h0g.22.37`).
///
/// The union over its four wiring-resolution statements in
/// `wamn_runtime::plugins::wamn_postgres::wiring_resolution`: `ACTIVE_WIRING_SQL`
/// (`wiring_activation`, `catalog_heads`, `wirings`, `wiring_tombstones`,
/// `component_library`), `RELEASE_WIRING_SQL` (`release_manifest_v2_snapshots`,
/// `wirings`, `release_components`, `component_library`), `CANDIDATE_WIRING_SQL`
/// and `RELEASE_COMPONENT_BINDINGS_READY_SQL` (the connection quartet). It is a
/// SUPERSET of [`MANAGEMENT_ADMITTER_CATALOG_RELATIONS`] and deliberately not
/// derived from it: the two surfaces agree on six relations today by
/// measurement, and deriving one from the other would make a change to either
/// statement silently widen the other family.
///
/// TABLE grain, not column grain — the [`grant_system_reader_surface_sql`]
/// determination: this family holds no write privilege anywhere in `catalog`,
/// so a column list would withhold only labels and timestamps while making a
/// statement that reads one more column fail in production instead of at review.
pub const EXECUTOR_PLATFORM_CATALOG_RELATIONS: [&str; 11] = [
    "wirings",
    "component_library",
    "connection_requirements",
    "connection_bindings",
    "connection_instances",
    "connection_generations",
    "catalog_heads",
    "release_components",
    "release_manifest_v2_snapshots",
    "wiring_activation",
    "wiring_tombstones",
];

/// Every `runs` column the executor-platform family WRITES, and no other
/// (`wamn-0h0g.22.37`).
///
/// Read as a compiler off the SIX statements that `UPDATE runs` under
/// [`crate::workload_role::WorkloadRoleFamily::ExecutorPlatform`] — the bead's
/// premise said five, and the sixth is real:
///
/// * `queue::sql::clear_pre_effect_state_sql` — `state_json`, `release_version`,
///   `manifest_digest`
/// * `queue::sql::grant_production_claim_sql` — `status`, `release_version`,
///   `manifest_digest`
/// * `queue::sql::terminalize_effect_uncertain_claim_sql` — `status`,
///   `fail_kind`, the caller-outcome family, `updated_at`
/// * `queue::sql::terminalize_exhausted_production_sql` — `status`,
///   `result_json`, the caller-outcome family, `updated_at`
/// * `transitions::release_caller_sql` — the caller-outcome family, `updated_at`
/// * `transitions::terminalize_sql` — `status`, `terminal_reason`,
///   `result_json`, `fail_kind`, `updated_at`
///
/// The SELECT side is deliberately NOT column-scoped: `transitions::FENCED_PREFIX`
/// locks the run with `SELECT r.* FROM runs AS r`, which needs SELECT on EVERY
/// column of `runs`. A column list there would withhold nothing and break the
/// fence, so relation-grain SELECT is the measured surface rather than a
/// concession. Confining the WRITE is what a column list can still buy, and it
/// buys a lot: this family matches the permissive `TO wamn_platform` floor arm,
/// so a blanket `UPDATE` would let one claim rewrite any tenant's admission pins.
pub const EXECUTOR_PLATFORM_RUN_UPDATE_COLUMNS: [&str; 14] = [
    "status",
    "terminal_reason",
    "fail_kind",
    "result_json",
    "state_json",
    "release_version",
    "manifest_digest",
    "caller_outcome_kind",
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
/// `queue::sql::renew_production_lease_sql` writes `lease_expires_at`. Nothing
/// writes `available_at`, `priority`, `stream_seq`, `max_attempts` or
/// `enqueued_at` — the FIFO position and the crash budget are admission's, and
/// a claim that could move them could starve or resurrect another tenant's run.
///
/// SELECT stays relation grain for the same reason it does on `runs`:
/// `transitions::FENCED_PREFIX` locks the queue row with
/// `SELECT q.* FROM run_queue AS q`. DELETE has no column grain in PostgreSQL at
/// all, so the two dequeue statements take it whole.
pub const EXECUTOR_PLATFORM_QUEUE_UPDATE_COLUMNS: [&str; 4] = [
    "lease_owner",
    "lease_expires_at",
    "lease_generation",
    "attempts",
];
/// `runs` columns read directly or through `RETURNING` by management admission.
///
/// `status` and `result_json` are the OBSERVATION LEG (wamn-oici). Without them
/// the admitter can create a run it can never see the outcome of, so a project
/// run cannot be polled to terminal and sequential case composition has no
/// credential that can execute it. The admitter is already the run's producer
/// principal, so widening its own SELECT is narrower than the alternatives a
/// second mechanism would need: a view, a separate reader role, or replicating
/// run status into the control database were all considered and rejected.
///
/// `caller_outcome_kind`, `caller_outcome_json`, `caller_http_status` and
/// `fail_kind` are the EVALUATION LEG (wamn-0h0g.8.25). They are exactly the
/// `runs` columns `wamn_scenario_model::Captured` is documented over, so they
/// are the facts `evaluate` consumes: `caller_http_status` plus
/// `caller_outcome_json` when `caller_outcome_kind` is `responded`, and
/// `fail_kind` when it is `failed`. Status alone says a run reached terminal,
/// not what it produced, so without these four a test-case run can be admitted
/// and polled to terminal and still never be evaluated.
///
/// `grant_management_admitter_surface_sql` revokes every table and per-column
/// privilege before granting, so this list is the whole readable surface —
/// a column absent here is DENIED, not merely unmentioned.
pub const MANAGEMENT_ADMITTER_RUN_SELECT_COLUMNS: [&str; 22] = [
    "tenant_id",
    "run_id",
    "binding_world_json",
    "idempotency_key",
    "trigger_source",
    "capture_mode",
    "catalog_id",
    "catalog_version",
    "environment",
    "wiring_id",
    "wiring_version",
    "wiring_hash",
    "input_json",
    "invocation_context",
    "platform_revision",
    "run_deadline_at",
    "status",
    "result_json",
    "caller_outcome_kind",
    "caller_outcome_json",
    "caller_http_status",
    "fail_kind",
];
/// `runs` columns minted by the management-admission statement.
pub const MANAGEMENT_ADMITTER_RUN_INSERT_COLUMNS: [&str; 18] = [
    "tenant_id",
    "run_id",
    "catalog_id",
    "catalog_version",
    "environment",
    "wiring_id",
    "wiring_version",
    "wiring_hash",
    "binding_world_json",
    "status",
    "trigger_source",
    "capture_mode",
    "input_json",
    "invocation_context",
    "admission_context_version",
    "platform_revision",
    "idempotency_key",
    "run_deadline_at",
];
/// `run_queue` columns observed through the management insert's `RETURNING`.
pub const MANAGEMENT_ADMITTER_QUEUE_SELECT_COLUMNS: [&str; 2] = ["tenant_id", "run_id"];
/// `run_queue` columns minted by management admission.
pub const MANAGEMENT_ADMITTER_QUEUE_INSERT_COLUMNS: [&str; 4] =
    ["tenant_id", "run_id", "available_at", "stream_seq"];

/// `REVOKE CONNECT ON DATABASE "<database>" FROM "wamn_dispatch_reader"`.
///
/// **This used to GRANT, and the reversal is the whole of `wamn-0h0g.22.24`.**
/// `wamn_dispatch_reader` is CLUSTER-GLOBAL and its generations are members
/// `WITH INHERIT TRUE`, so one `GRANT CONNECT` per environment reached EVERY
/// environment on the cluster — the identical defect `wamn-0h0g.12.179` measured
/// live for the guest and closed in [`grant_connect_on_database_sql`]'s caller.
/// CONNECT belongs to the GENERATION, which
/// [`prepare_workload_generation_sql`] grants it directly and only on the one
/// database that generation was minted for.
///
/// **Order is load-bearing exactly as it is for the owner statement:** run this
/// AFTER [`set_database_owner_sql`]. `ALTER DATABASE … OWNER TO` rewrites the
/// outgoing owner's ACL entry, and a revoke applied before it can be undone by
/// the entry the owner change carries over.
///
/// It is what CONVERGES a pre-cutover environment: an environment provisioned
/// while the reader was a stable LOGIN still carries that `CONNECT`, and the
/// dispatch-reader generation prepare refuses until this has run, because
/// `verify_stable_workload_role` requires a connection-free stable ACL role.
pub fn revoke_dispatch_reader_connect_sql(database: &str) -> String {
    format!(
        "REVOKE CONNECT ON DATABASE {db} FROM {role};",
        db = quote_ident(database),
        role = quote_ident(DISPATCH_READER_ROLE),
    )
}

/// The dispatcher's whole in-database read surface, applied convergently inside
/// one project-env database: `USAGE` on the run-plane `schema` and `SELECT` on
/// [`DISPATCH_READER_RELATIONS`]. Nothing else — no `runs`, no `node_runs`, no
/// `catalog`, no `EXECUTE`.
///
/// Every grant is preceded by a blanket `REVOKE` over the same scope, so the
/// batch NARROWS as well as grants: an environment where someone widened the
/// reader converges back to exactly this surface on the next apply, and a replay
/// against an already-correct environment is a no-op. That is what makes this the
/// provisioner's convergent step rather than a one-shot migration script.
///
/// Run connected to the project-env database as a principal that owns the
/// run-plane relations (the database owner or the cluster superuser), AFTER the
/// run-plane schema has been applied — the `REVOKE`/`GRANT` name relations that
/// must already exist.
pub fn grant_dispatch_reader_read_surface_sql(schema: &str) -> String {
    let role = quote_ident(DISPATCH_READER_ROLE);
    let schema_ident = quote_ident(schema);
    let mut sql = format!(
        "REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA {schema_ident} FROM {role}; \
         REVOKE ALL PRIVILEGES ON SCHEMA {schema_ident} FROM {role}; \
         GRANT USAGE ON SCHEMA {schema_ident} TO {role};"
    );
    for relation in DISPATCH_READER_RELATIONS {
        sql.push_str(&format!(
            " GRANT SELECT ON {schema_ident}.{relation} TO {role};",
            relation = quote_ident(relation),
        ));
    }
    sql
}

/// Idempotently create the stable effect-ledger ACL role as NOLOGIN.
///
/// Table/schema grants deliberately do not live here: schema-control owns them
/// once the effect-ledger tables exist. This builder only establishes the
/// cluster-global, ownership-free role identity and restrictive attributes.
pub fn ensure_effect_writer_acl_role_sql() -> String {
    ensure_workload_acl_role_sql(WorkloadRoleFamily::EffectWriter)
}

/// Idempotently create or harden one stable workload ACL role as NOLOGIN.
pub fn ensure_workload_acl_role_sql(family: WorkloadRoleFamily) -> String {
    let role = family.acl_role();
    format!(
        "DO $workload_acl$ DECLARE role_name text := {role_lit}; BEGIN \
           PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN \
             EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
           ELSIF EXISTS (SELECT FROM pg_roles WHERE rolname = role_name \
                         AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole \
                              OR rolinherit OR rolreplication OR rolbypassrls \
                              OR rolpassword IS NOT NULL)) THEN \
             EXECUTE format('ALTER ROLE %I NOLOGIN PASSWORD NULL NOSUPERUSER NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
           END IF; \
         END $workload_acl$;",
        role_lit = quote_literal(role),
    )
}

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
                              OR rolinherit OR rolreplication OR rolbypassrls \
                              OR rolpassword IS NOT NULL)) THEN \
             EXECUTE format('ALTER ROLE %I NOLOGIN PASSWORD NULL NOSUPERUSER NOCREATEDB \
               NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
           END IF; \
         EXCEPTION WHEN duplicate_object THEN NULL; \
         END $platform_group$;",
        role_lit = quote_literal(PLATFORM_GROUP_ROLE),
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
/// wamn_effect_writer` lands `inherit_option = false` and the two-hop chain
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
/// # IT DOES NOT ENSURE THE ACL ROLE
///
/// Historically it could not: [`WorkloadRoleFamily::DispatchReader`]'s stable
/// role was minted as a LOGIN with a password, and [`ensure_workload_acl_role_sql`]
/// HARDENS its target to `NOLOGIN PASSWORD NULL`, so composing the ensure builder
/// in here would have revoked that credential every time this edge converged.
/// `wamn-0h0g.22.24` retired that shape and the hazard with it. The separation
/// stays anyway, because it is the right one: an ACL role's lifecycle belongs to
/// its own builder, and this one only touches the edge — behind an existence
/// guard, so a caller that has not created the role yet gets a no-op rather than
/// an error.
pub fn platform_group_membership_sql(family: WorkloadRoleFamily) -> String {
    let acl_role = quote_ident(family.acl_role());
    let acl_role_lit = quote_literal(family.acl_role());
    let group = quote_ident(PLATFORM_GROUP_ROLE);
    let grant = if family.is_platform_grain() {
        format!(" GRANT {group} TO {acl_role} WITH ADMIN FALSE, INHERIT TRUE, SET FALSE;")
    } else {
        String::new()
    };
    format!(
        "{ensure_group} \
         DO $platform_edge$ BEGIN \
           IF EXISTS (SELECT FROM pg_roles WHERE rolname = {acl_role_lit}) THEN \
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
        schema = quote_ident(crate::tenant_key::TENANT_KEY_SCHEMA),
        role = quote_ident(role),
    )
}

/// Converge the stable management-admitter role to its exact admission surface.
///
/// This is the seventh [`WorkloadRoleFamily`]'s one family-specific privilege
/// step. Its A/B identities and their prepare/retire lifecycle remain wholly in
/// the generic workload machinery. The catalog relations are the surviving
/// component/wiring facts read by management admission; the run-plane grants
/// are column-exact to the ordinary run-plus-queue statement. Environment
/// policy access is read-only because the invoker trigger resolves the pinned
/// durability class while inserting a run.
///
/// Blanket revocation precedes every grant so reapplying this batch removes a
/// stale or widened direct ACL instead of merely adding the intended surface.
pub fn grant_management_admitter_surface_sql(schema: &str) -> String {
    let role = quote_ident(MANAGEMENT_ADMITTER_ROLE);
    let schema_literal = quote_literal(schema);
    let schema = quote_ident(schema);
    let mut sql = format!(
        "{ensure} \
         REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, {schema} FROM {role}; \
         DO $management_column_acl$ DECLARE target record; BEGIN \
           FOR target IN \
             SELECT namespace.nspname, relation.relname, attribute.attname \
               FROM pg_catalog.pg_attribute AS attribute \
               JOIN pg_catalog.pg_class AS relation ON relation.oid = attribute.attrelid \
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
              WHERE namespace.nspname IN ('catalog', {schema_literal}) \
                AND relation.relkind IN ('r', 'p') \
                AND attribute.attnum > 0 AND NOT attribute.attisdropped \
           LOOP \
             EXECUTE format( \
               'REVOKE ALL PRIVILEGES (%I) ON TABLE %I.%I FROM %I', \
               target.attname, target.nspname, target.relname, {role_literal}); \
           END LOOP; \
         END $management_column_acl$; \
         REVOKE ALL PRIVILEGES ON SCHEMA catalog, {schema} FROM {role}; \
         GRANT USAGE ON SCHEMA catalog, {schema} TO {role};",
        ensure = ensure_workload_acl_role_sql(WorkloadRoleFamily::ManagementAdmitter),
        role_literal = quote_literal(MANAGEMENT_ADMITTER_ROLE),
    );
    for relation in MANAGEMENT_ADMITTER_CATALOG_RELATIONS {
        sql.push_str(&format!(
            " GRANT SELECT ON TABLE catalog.{relation} TO {role};",
            relation = quote_ident(relation),
        ));
    }
    let run_select = quoted_column_list(&MANAGEMENT_ADMITTER_RUN_SELECT_COLUMNS);
    let run_insert = quoted_column_list(&MANAGEMENT_ADMITTER_RUN_INSERT_COLUMNS);
    let queue_select = quoted_column_list(&MANAGEMENT_ADMITTER_QUEUE_SELECT_COLUMNS);
    let queue_insert = quoted_column_list(&MANAGEMENT_ADMITTER_QUEUE_INSERT_COLUMNS);
    sql.push_str(&format!(
        " GRANT SELECT ON TABLE {schema}.\"environment_policies\" TO {role}; \
         GRANT SELECT ({run_select}) ON TABLE {schema}.\"runs\" TO {role}; \
         GRANT INSERT ({run_insert}) ON TABLE {schema}.\"runs\" TO {role}; \
         GRANT SELECT ({queue_select}), INSERT ({queue_insert}) \
           ON TABLE {schema}.\"run_queue\" TO {role};"
    ));
    // `runs` carries `runs_tkey`, so the INSERT above is dead without this.
    sql.push(' ');
    sql.push_str(&grant_tenant_key_execute_sql(MANAGEMENT_ADMITTER_ROLE));
    sql
}

fn quoted_column_list(columns: &[&str]) -> String {
    columns
        .iter()
        .map(|column| quote_ident(column))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Converge the stable callable-HTTP admitter role to its exact read surface
/// (`wamn-0h0g.22.37`).
///
/// # The surface is ONE statement, and that is the whole finding
///
/// `AuthorityClass::CallableHttp` is selected from exactly one place in
/// production — `WamnPostgres::connection_effect_snapshot` — and that method
/// issues exactly one statement, `CONNECTION_EFFECT_SNAPSHOT_SQL`. Its `FROM`
/// and `LEFT JOIN` clauses name [`MANAGEMENT_ADMITTER_CATALOG_RELATIONS`] and
/// nothing else, so `USAGE` on `catalog` plus six `SELECT`s is the family's
/// TOTAL authority anywhere in the cluster.
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
///   forms an index entry, and the one statement carries no predicate over the
///   derivation for the planner to evaluate.
/// * NO function EXECUTE at all. The statement calls only built-ins.
///
/// Run connected to the project-environment database as a principal owning both
/// schemas (the database owner or the cluster superuser), AFTER
/// `deploy/sql/catalog-schema.sql` and the run-plane files have been applied —
/// the `REVOKE`/`GRANT` name relations that must already exist.
pub fn grant_http_admitter_surface_sql(schema: &str) -> String {
    let role = quote_ident(HTTP_ADMITTER_ROLE);
    let schema = quote_ident(schema);
    let mut sql = format!(
        "{ensure} \
         REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, {schema} FROM {role}; \
         REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, {schema} FROM {role}; \
         REVOKE ALL PRIVILEGES ON SCHEMA catalog, {schema} FROM {role}; \
         GRANT USAGE ON SCHEMA catalog TO {role};",
        ensure = ensure_workload_acl_role_sql(WorkloadRoleFamily::HttpAdmitter),
    );
    for relation in MANAGEMENT_ADMITTER_CATALOG_RELATIONS {
        sql.push_str(&format!(
            " GRANT SELECT ON TABLE catalog.{relation} TO {role};",
            relation = quote_ident(relation),
        ));
    }
    sql
}

/// Converge the stable executor-platform role to its exact claim surface
/// (`wamn-0h0g.22.37`).
///
/// Derived from the NINE production call sites that select
/// `AuthorityClass::ExecutorPlatform`: four in
/// `wamn_runtime::plugins::wamn_postgres::production_claim`, four in the same
/// module's `wiring_resolution` sibling, and the wiring-doorbell `LISTEN` in
/// `claims` — which holds a connection open and names no relation, so it adds
/// nothing here.
///
/// # The grains, and why each is the one it is
///
/// * `catalog`: `USAGE` plus TABLE-grain `SELECT` on
///   [`EXECUTOR_PLATFORM_CATALOG_RELATIONS`]. No write of any kind.
/// * `runs`: TABLE-grain `SELECT` because `transitions::FENCED_PREFIX` locks the
///   row with `SELECT r.*`, and COLUMN-grain `UPDATE` over
///   [`EXECUTOR_PLATFORM_RUN_UPDATE_COLUMNS`]. The admission pins, the wiring
///   identity, the input and the durability class are all UNREACHABLE by this
///   family as a result — which matters because the permissive
///   `TO wamn_platform` floor arm is `USING (true)`, so a blanket `UPDATE` here
///   would be a cross-tenant rewrite privilege.
/// * `run_queue`: TABLE-grain `SELECT` (`SELECT q.*` in the same fence) and
///   `DELETE` (PostgreSQL has no column-grain DELETE), plus COLUMN-grain
///   `UPDATE` over [`EXECUTOR_PLATFORM_QUEUE_UPDATE_COLUMNS`] — the lease and
///   the crash counter, never the FIFO position.
/// * `effect_attempts`: `SELECT` only. The ledger is the effect writer's to
///   append to; the claim path only asks whether a row exists.
/// * `wamn_run.require_executor_platform_authority()`: every one of the ten
///   run-plane statements opens with it. `deploy/sql/run-state.sql` leaves
///   PUBLIC's default EXECUTE in place today, so this grant changes no answer
///   yet — it is what keeps the family reaching its own guard the day PUBLIC is
///   revoked, and it is the only reason the guard is not silently PUBLIC
///   authority in the denial matrix.
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
    let role = quote_ident(EXECUTOR_PLATFORM_ROLE);
    let schema = quote_ident(schema);
    let mut sql = format!(
        "{ensure} \
         REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, {schema} FROM {role}; \
         REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, {schema} FROM {role}; \
         REVOKE ALL PRIVILEGES ON SCHEMA catalog, {schema} FROM {role}; \
         GRANT USAGE ON SCHEMA catalog, {schema} TO {role};",
        ensure = ensure_workload_acl_role_sql(WorkloadRoleFamily::ExecutorPlatform),
    );
    for relation in EXECUTOR_PLATFORM_CATALOG_RELATIONS {
        sql.push_str(&format!(
            " GRANT SELECT ON TABLE catalog.{relation} TO {role};",
            relation = quote_ident(relation),
        ));
    }
    let run_update = quoted_column_list(&EXECUTOR_PLATFORM_RUN_UPDATE_COLUMNS);
    let queue_update = quoted_column_list(&EXECUTOR_PLATFORM_QUEUE_UPDATE_COLUMNS);
    sql.push_str(&format!(
        " GRANT SELECT ON TABLE {schema}.\"runs\" TO {role}; \
         GRANT UPDATE ({run_update}) ON TABLE {schema}.\"runs\" TO {role}; \
         GRANT SELECT, DELETE ON TABLE {schema}.\"run_queue\" TO {role}; \
         GRANT UPDATE ({queue_update}) ON TABLE {schema}.\"run_queue\" TO {role}; \
         GRANT SELECT ON TABLE {schema}.\"effect_attempts\" TO {role}; \
         GRANT EXECUTE ON FUNCTION \
           {schema}.\"require_executor_platform_authority\"() TO {role};"
    ));
    // `runs` carries `runs_tkey`, so the column-exact UPDATE above is dead
    // without this on every write that changes an indexed column.
    sql.push(' ');
    sql.push_str(&grant_tenant_key_execute_sql(EXECUTOR_PLATFORM_ROLE));
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

/// The one relation the CDC reader's registration `SELECT` touches.
///
/// `services/cdc-reader` runs `wamn_control_registry::sql::select_event_reader_sql`
/// over `WAMN_SYSTEM_URL` and nothing else, so this is its whole authority.
pub const REGISTRY_READER_RELATIONS: [&str; 1] = ["event_readers"];

/// The relations the management surface's two identity `SELECT`s touch.
///
/// `SELECT_PAT_BY_PREFIX_SQL` joins `pats` to `principals`; `SELECT_PROJECT_ROLES_SQL`
/// reads `project_roles` (both in `crates/identity/platform/src/lib.rs`).
/// PostgreSQL checks privileges on every relation a statement references, so all
/// three are load bearing.
pub const IDENTITY_READER_RELATIONS: [&str; 3] = ["pats", "principals", "project_roles"];

/// Converge one system-plane reader's stable ACL role to exactly `SELECT` on
/// `relations` inside `schema`, and nothing anywhere else in the control plane.
///
/// Table-level rather than column-exact — the
/// [`grant_dispatch_reader_read_surface_sql`] shape rather than the
/// management-admitter one: these roles hold no write privilege at all, so a
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
    let role = quote_ident(family.acl_role());
    let plane = SYSTEM_PLANE_SCHEMAS
        .iter()
        .map(|name| quote_ident(name))
        .collect::<Vec<_>>()
        .join(", ");
    let schema = quote_ident(schema);
    let mut sql = format!(
        "{ensure} \
         REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA {plane} FROM {role}; \
         REVOKE ALL PRIVILEGES ON SCHEMA {plane} FROM {role}; \
         GRANT USAGE ON SCHEMA {schema} TO {role};",
        ensure = ensure_workload_acl_role_sql(family),
    );
    for relation in relations {
        sql.push_str(&format!(
            " GRANT SELECT ON TABLE {schema}.{relation} TO {role};",
            relation = quote_ident(relation),
        ));
    }
    sql
}

/// Converge `wamn_registry_reader` to its exact CDC-registration read surface.
///
/// One `SELECT`, on `registry.event_readers`. The role is refused on `identity.*`
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
/// `SELECT` on three `identity` relations, and NO `INSERT` and NO `UPDATE`
/// anywhere. Both consuming statements are plain reads that take no row lock, so
/// a genuinely SELECT-only role serves the surface as it stands.
///
/// **SELECT-only is correct only because `POST /login` no longer exists.** The
/// live cluster role carries `SELECT, INSERT, UPDATE`; its `INSERT` was added
/// deliberately when that route did exist, and the grant then drifted
/// SELECT-only -> +INSERT -> +INSERT,UPDATE out of band THREE times.
/// `services/scenario-worker`'s
/// `only_the_mounted_kinds_have_a_route_and_the_rest_answer_a_bare_501` decides
/// the mounted inventory by CALLING route selection, so a re-mounted session
/// route changes an answer that test reads. The write privilege such a route
/// would need is pinned separately by
/// `the_identity_reader_grants_no_insert_and_no_update`, so the fourth drift
/// fails a test instead of landing.
pub fn grant_identity_reader_surface_sql() -> String {
    grant_system_reader_surface_sql(
        WorkloadRoleFamily::IdentityReader,
        "identity",
        &IDENTITY_READER_RELATIONS,
    )
}

/// Prepare one inactive scoped credential generation for authenticated use.
///
/// `role` is the validated deterministic generation name. The caller verifies
/// the slot is inactive before applying this batch. Password and server-side
/// `VALID UNTIL` are replaced, new membership is the stable effect ACL role, and
/// direct database authority is solely `CONNECT` on this project database.
pub fn prepare_effect_writer_generation_sql(
    database: &str,
    role: &str,
    password: &str,
    expires_at: &str,
) -> String {
    prepare_workload_generation_sql(
        WorkloadRoleFamily::EffectWriter,
        database,
        role,
        password,
        expires_at,
    )
}

/// The family-specific privilege batch that converges one family's STABLE ACL
/// role to its exact grant set, when this crate owns that convergence.
///
/// `None` = the family's stable role carries no grant set this batch applies.
/// The effect writer is the standing example: schema-control owns its grants
/// because they only exist once the effect-ledger tables do, so this batch has
/// nothing to converge and a caller must not assert the grant set before
/// preparing. An admitted family lands on `None` with no edit here; a family
/// that acquires a grant set adds it beside its builder, which is the one place
/// a family stays per-family (`wamn-0h0g.22.16`).
pub fn stable_surface_sql(family: WorkloadRoleFamily) -> Option<String> {
    match family {
        WorkloadRoleFamily::ManagementAdmitter => {
            Some(grant_management_admitter_surface_sql("wamn_run"))
        }
        WorkloadRoleFamily::ExecutorPlatform => {
            Some(grant_executor_platform_surface_sql("wamn_run"))
        }
        WorkloadRoleFamily::HttpAdmitter => Some(grant_http_admitter_surface_sql("wamn_run")),
        WorkloadRoleFamily::RegistryReader => Some(grant_registry_reader_surface_sql()),
        WorkloadRoleFamily::IdentityReader => Some(grant_identity_reader_surface_sql()),
        // THE MEASURED SURFACE IS EMPTY, and the owner's determination is that
        // an empty surface is served by NOT EXISTING (`wamn-0h0g.22.37`).
        //
        // `AuthorityClass::EventMaterializer` maps to this family and nothing
        // in production selects it: there is no materializer consumer, so there
        // is no statement, so there is no relation to name. The family stays an
        // enum variant so the closed vocabulary stays complete and an admitted
        // consumer inherits the whole lifecycle without an edit here.
        //
        // A revoke-only builder was considered and REFUSED. Credentials, a
        // Secret and rotation machinery for a role that can reach nothing is
        // provisioning for no consumer, and "reaches exactly its measured
        // surface" is already satisfied by holding no grant at all.
        //
        // THE TRIGGER FOR MINTING A REAL BUILDER IS THE FIRST PRODUCTION
        // MATERIALIZER CONSUMER — not a plan for one, and not a test fixture.
        WorkloadRoleFamily::EventMaterializer => None,
        _ => None,
    }
}

/// Prepare one inactive generation for a closed workload family.
pub fn prepare_workload_generation_sql(
    family: WorkloadRoleFamily,
    database: &str,
    role: &str,
    password: &str,
    expires_at: &str,
) -> String {
    let role_ident = quote_ident(role);
    let role_lit = quote_literal(role);
    let membership = normalize_workload_generation_membership_sql(family, role, true);
    let stable_surface =
        stable_surface_sql(family).unwrap_or_else(|| ensure_workload_acl_role_sql(family));
    format!(
        "{stable_surface} \
         DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = {role_lit}) THEN \
             CREATE ROLE {role_ident} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               INHERIT NOREPLICATION NOBYPASSRLS; \
           END IF; \
         END $$; \
         {membership} \
         ALTER ROLE {role_ident} LOGIN PASSWORD {password} VALID UNTIL {expires_at}; \
         GRANT CONNECT ON DATABASE {database} TO {role_ident};",
        stable_surface = stable_surface,
        password = quote_literal(password),
        expires_at = quote_literal(expires_at),
        database = quote_ident(database),
    )
}

/// Normalize one existing generation's membership during lifecycle migration.
///
/// `active` selects the one exact stable-role edge or no edge. The effect-writer
/// arm also removes the retired projection membership; it is migration input,
/// never a generic family.
pub fn normalize_workload_generation_membership_sql(
    family: WorkloadRoleFamily,
    role: &str,
    active: bool,
) -> String {
    let role_ident = quote_ident(role);
    let legacy_projection_revoke = if family == WorkloadRoleFamily::EffectWriter {
        format!(
            "DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = {legacy_lit}) THEN \
               REVOKE {legacy_ident} FROM {role_ident}; END IF; END $$;",
            legacy_lit = quote_literal(wamn_run_state::RUN_PROJECTION_WRITER_ROLE),
            legacy_ident = quote_ident(wamn_run_state::RUN_PROJECTION_WRITER_ROLE),
        )
    } else {
        String::new()
    };
    let grant = if active {
        format!(
            "GRANT {acl_role} TO {role_ident} \
               WITH ADMIN FALSE, INHERIT TRUE, SET FALSE;",
            acl_role = quote_ident(family.acl_role()),
        )
    } else {
        String::new()
    };
    format!(
        "{ensure} \
         {platform_arm} \
         {legacy_projection_revoke} \
         REVOKE {acl_role} FROM {role_ident}; \
         {grant}",
        ensure = ensure_workload_acl_role_sql(family),
        platform_arm = platform_group_membership_sql(family),
        legacy_projection_revoke = legacy_projection_revoke,
        acl_role = quote_ident(family.acl_role()),
        grant = grant,
    )
}

/// Retire one old credential generation after replacement use was verified.
///
/// The batch removes authority and then authentication. The caller commits it
/// before terminating sessions with [`terminate_effect_writer_generation_sessions_sql`].
pub fn retire_effect_writer_generation_sql(database: &str, role: &str) -> String {
    retire_workload_generation_sql(WorkloadRoleFamily::EffectWriter, database, role)
}

/// Remove one workload generation's authority before disabling authentication.
pub fn retire_workload_generation_sql(
    family: WorkloadRoleFamily,
    database: &str,
    role: &str,
) -> String {
    let role_ident = quote_ident(role);
    format!(
        "REVOKE {acl_role} FROM {role_ident}; \
         REVOKE CONNECT ON DATABASE {database} FROM {role_ident}; \
         ALTER ROLE {role_ident} NOLOGIN PASSWORD NULL VALID UNTIL 'epoch';",
        acl_role = quote_ident(family.acl_role()),
        database = quote_ident(database),
    )
}

/// Terminate sessions only after credential authority removal has committed.
pub fn terminate_effect_writer_generation_sessions_sql(role: &str) -> String {
    terminate_workload_generation_sessions_sql(role)
}

/// Terminate sessions only after a workload generation is retired.
pub fn terminate_workload_generation_sessions_sql(role: &str) -> String {
    let role_lit = quote_literal(role);
    format!(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
           WHERE usename = {role_lit} AND pid <> pg_backend_pid();"
    )
}

/// Read-only state probe used by ctl and the bootstrap wrapper.
///
/// `$1` is the exact generation role. The result exposes authentication, exact
/// direct role memberships, exact direct database CONNECT ACLs, and active
/// session count without carrying password material.
pub fn effect_writer_generation_state_sql() -> &'static str {
    workload_generation_state_sql()
}

/// Read one generation or stable ACL role without family-wide cardinality assumptions.
pub fn workload_generation_state_sql() -> &'static str {
    "SELECT r.rolcanlogin, r.rolsuper, r.rolinherit, r.rolcreaterole, r.rolcreatedb, \
            r.rolreplication, r.rolbypassrls, \
            r.rolpassword IS NOT NULL AS password_set, \
            CASE WHEN r.rolvaliduntil IS NULL THEN NULL \
                 ELSE to_char(r.rolvaliduntil AT TIME ZONE 'UTC', \
                              'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') END AS valid_until, \
            COALESCE(isfinite(r.rolvaliduntil), false) AS valid_until_finite, \
            COALESCE((SELECT array_agg(parent.rolname::text ORDER BY parent.rolname::text) \
                        FROM pg_auth_members m JOIN pg_roles parent ON parent.oid = m.roleid \
                       WHERE m.member = r.oid), ARRAY[]::text[]) AS memberships, \
            COALESCE((SELECT bool_and(NOT m.admin_option AND m.inherit_option AND NOT m.set_option) \
                        FROM pg_auth_members m WHERE m.member = r.oid), true) \
              AS membership_options_exact, \
            COALESCE((SELECT bool_and(NOT m.admin_option AND m.inherit_option) \
                        FROM pg_auth_members m WHERE m.member = r.oid), true) \
              AS membership_options_migratable, \
            COALESCE((SELECT array_agg(member.rolname::text ORDER BY member.rolname::text) \
                        FROM pg_auth_members m JOIN pg_roles member ON member.oid = m.member \
                       WHERE m.roleid = r.oid), ARRAY[]::text[]) AS member_roles, \
            COALESCE((SELECT bool_and(NOT m.admin_option AND m.inherit_option AND NOT m.set_option) \
                        FROM pg_auth_members m WHERE m.roleid = r.oid), true) \
              AS member_options_exact, \
            NOT EXISTS ( \
              SELECT 1 FROM pg_auth_members child_edge \
              JOIN pg_roles generation ON generation.oid = child_edge.member \
              WHERE child_edge.roleid = r.oid AND ( \
                child_edge.admin_option OR NOT child_edge.inherit_option OR child_edge.set_option \
                OR NOT generation.rolcanlogin OR generation.rolsuper \
                OR generation.rolcreatedb OR generation.rolcreaterole \
                OR NOT generation.rolinherit OR generation.rolreplication \
                OR generation.rolbypassrls OR generation.rolpassword IS NULL \
                OR generation.rolvaliduntil IS NULL OR NOT isfinite(generation.rolvaliduntil) \
                OR (SELECT count(*) FROM pg_auth_members parent_edge \
                     WHERE parent_edge.member = generation.oid) <> 1 \
                OR EXISTS (SELECT 1 FROM pg_auth_members parent_edge \
                            WHERE parent_edge.member = generation.oid \
                              AND (parent_edge.admin_option \
                                   OR NOT parent_edge.inherit_option \
                                   OR parent_edge.set_option)) \
                OR EXISTS (SELECT 1 FROM pg_auth_members grandchild_edge \
                            WHERE grandchild_edge.roleid = generation.oid) \
                OR EXISTS (SELECT 1 FROM pg_shdepend dependency \
                            WHERE dependency.refclassid = 'pg_authid'::regclass \
                              AND dependency.refobjid = generation.oid \
                              AND dependency.deptype = 'o') \
                OR (SELECT count(*) FROM pg_database direct_database \
                    CROSS JOIN LATERAL aclexplode(direct_database.datacl) direct_acl \
                    WHERE direct_acl.grantee = generation.oid) <> 1 \
                OR EXISTS (SELECT 1 FROM pg_database direct_database \
                           CROSS JOIN LATERAL aclexplode(direct_database.datacl) direct_acl \
                           WHERE direct_acl.grantee = generation.oid \
                             AND (direct_acl.privilege_type <> 'CONNECT' \
                                  OR direct_acl.is_grantable)))) \
              AS generation_children_exact, \
            COALESCE((SELECT array_agg(d.datname::text ORDER BY d.datname::text) \
                        FROM pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) acl \
                       WHERE acl.grantee = r.oid AND acl.privilege_type = 'CONNECT'), \
                     ARRAY[]::text[]) AS connect_databases, \
            (SELECT count(*)::bigint FROM pg_stat_activity a WHERE a.usename = r.rolname) AS sessions, \
            (SELECT count(*)::bigint FROM pg_shdepend d \
              WHERE d.refclassid = 'pg_authid'::regclass AND d.refobjid = r.oid \
                AND d.deptype = 'o') AS owned_objects \
       FROM pg_catalog.pg_authid r WHERE r.rolname = $1"
}

/// Read-only proof that no CONNECTABLE database grants `CONNECT` to PUBLIC.
///
/// The filter is `datallowconn`, not `NOT datistemplate`, and must stay so:
/// `template1` is a template AND connectable, so a template filter reported a
/// clean floor while `template1` still carried PostgreSQL's default PUBLIC
/// `CONNECT`. `template0` (`datallowconn = false`) is correctly out of scope —
/// it keeps its PUBLIC `CONNECT` aclitem, which no session can use.
pub fn public_connect_databases_sql() -> &'static str {
    "SELECT d.datname::text FROM pg_database d \
      WHERE d.datallowconn AND EXISTS ( \
        SELECT FROM aclexplode(COALESCE(d.datacl, acldefault('d', d.datdba))) acl \
         WHERE acl.grantee = 0 AND acl.privilege_type = 'CONNECT') \
      ORDER BY d.datname::text"
}

/// Revoke PUBLIC `CONNECT` from every CONNECTABLE database in this cluster.
///
/// Database privileges are cluster catalog entries, so the DO block may target
/// each database while connected to the exact project database. This gives ctl
/// ownership of converging the ratified cluster-wide floor during initial
/// generation preparation.
///
/// The loop selects on `datallowconn`, **not** `NOT datistemplate`. `template1`
/// is a template with `datallowconn = true`, so a template filter left its
/// default PUBLIC `CONNECT` untouched — and a `template1` session is a live
/// session on the cluster from which any database that principal OWNS can be
/// `ALTER`ed or `DROP`ped, needing no `CONNECT` on the target at all.
/// `template0` is `datallowconn = false` and stays untouched, as it must:
/// `CREATE DATABASE … TEMPLATE template1` does not require the creator to hold
/// `CONNECT`, so closing the route costs no provisioning capability.
pub fn revoke_public_connect_floor_sql() -> &'static str {
    "DO $$ DECLARE database_name text; BEGIN \
       FOR database_name IN SELECT datname FROM pg_database WHERE datallowconn LOOP \
         EXECUTE format('REVOKE CONNECT ON DATABASE %I FROM PUBLIC', database_name); \
       END LOOP; \
     END $$;"
}

/// Read-only proof of PUBLIC's effective `TEMPORARY` privilege on the target.
///
/// The caller connects to the already-validated exact project database. Unlike
/// the cluster-wide `CONNECT` floor, `TEMPORARY` is deliberately confined only
/// on that database so unrelated databases retain their own policy.
pub fn public_temporary_on_current_database_sql() -> &'static str {
    "SELECT EXISTS (SELECT FROM pg_database d CROSS JOIN LATERAL \
       aclexplode(COALESCE(d.datacl, acldefault('d', d.datdba))) acl \
      WHERE d.datname = current_database() AND acl.grantee = 0 \
        AND acl.privilege_type = 'TEMPORARY')"
}

/// All non-template databases available for exact cross-database ACL proof.
///
/// This one keeps the `NOT datistemplate` filter on purpose, unlike
/// [`public_connect_databases_sql`] / [`revoke_public_connect_floor_sql`]: the
/// caller OPENS a session against each name it returns, and an open session on
/// `template1` makes concurrent `CREATE DATABASE … TEMPLATE template1` fail.
/// The floor builders only read and revoke catalog ACLs, so they can and must
/// cover `template1`; this one cannot.
pub fn non_template_databases_sql() -> &'static str {
    "SELECT datname::text FROM pg_database WHERE NOT datistemplate ORDER BY datname::text"
}

/// Read-only direct ACL inventory for one role in the connected database.
pub fn role_database_acl_inventory_sql() -> &'static str {
    "WITH wanted AS (SELECT oid FROM pg_roles WHERE rolname = $1), acl AS ( \
       SELECT 'database'::text AS object_kind, d.datname::text AS schema_name, \
              d.datname::text AS object_name, x.privilege_type::text, x.is_grantable \
         FROM pg_database d CROSS JOIN LATERAL \
              aclexplode(COALESCE(d.datacl, acldefault('d', d.datdba))) x \
        WHERE d.datname = current_database() AND x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'schema', n.nspname::text, n.nspname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_namespace n CROSS JOIN LATERAL \
              aclexplode(COALESCE(n.nspacl, acldefault('n', n.nspowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'relation', n.nspname::text, c.relname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         CROSS JOIN LATERAL aclexplode(COALESCE(c.relacl, acldefault('r', c.relowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'routine', n.nspname::text, p.proname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
         CROSS JOIN LATERAL aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'column', n.nspname::text, c.relname::text || '.' || a.attname::text, \
              x.privilege_type::text, x.is_grantable \
         FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         CROSS JOIN LATERAL aclexplode(a.attacl) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'type', n.nspname::text, t.typname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
         CROSS JOIN LATERAL aclexplode(COALESCE(t.typacl, acldefault('T', t.typowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'language', ''::text, l.lanname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_language l CROSS JOIN LATERAL \
              aclexplode(COALESCE(l.lanacl, acldefault('l', l.lanowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'large-object', ''::text, l.oid::text, x.privilege_type::text, x.is_grantable \
         FROM pg_largeobject_metadata l CROSS JOIN LATERAL \
              aclexplode(COALESCE(l.lomacl, acldefault('L', l.lomowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'foreign-data-wrapper', ''::text, f.fdwname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_foreign_data_wrapper f CROSS JOIN LATERAL \
              aclexplode(COALESCE(f.fdwacl, acldefault('F', f.fdwowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'foreign-server', ''::text, s.srvname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_foreign_server s CROSS JOIN LATERAL \
              aclexplode(COALESCE(s.srvacl, acldefault('S', s.srvowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'tablespace', ''::text, t.spcname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_tablespace t CROSS JOIN LATERAL \
              aclexplode(COALESCE(t.spcacl, acldefault('t', t.spcowner))) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'parameter', ''::text, p.parname::text, x.privilege_type::text, x.is_grantable \
         FROM pg_parameter_acl p CROSS JOIN LATERAL aclexplode(p.paracl) x \
        WHERE x.grantee = (SELECT oid FROM wanted) \
       UNION ALL \
       SELECT 'default-acl', COALESCE(n.nspname::text, ''), d.defaclobjtype::text, \
              x.privilege_type::text, x.is_grantable \
         FROM pg_default_acl d LEFT JOIN pg_namespace n ON n.oid = d.defaclnamespace \
         CROSS JOIN LATERAL aclexplode(d.defaclacl) x \
        WHERE x.grantee = (SELECT oid FROM wanted)) \
     SELECT object_kind, schema_name, object_name, privilege_type, is_grantable FROM acl \
      ORDER BY object_kind, schema_name, object_name, privilege_type"
}

/// Session-scoped serialization primitive retained for the effect-writer caller.
pub fn effect_writer_scope_lock_sql() -> &'static str {
    workload_scope_lock_sql()
}

/// Session-scoped serialization primitive for workload credential mutation.
///
/// The generic lifecycle supplies a family-global key because it normalizes
/// every current member of that family's stable ACL role. Serializing at a
/// narrower scope would let a stale cross-scope read regrant a retired member.
pub fn workload_scope_lock_sql() -> &'static str {
    "SELECT pg_advisory_lock(hashtextextended($1::text, 0))"
}

// --- Control-database author provisioning (wamn-0h0g.8.18) --------------------
//
// The management service's authoring/report store moved to the control database.
// Its principal is a scoped A/B LOGIN generation of the stable NOLOGIN
// [`CONTROL_AUTHOR_ROLE`]; `deploy/sql/control-portable-store.sql` owns what that
// role may touch, and these builders own the role identities themselves.
//
// The public family-specific functions below delegate to the closed generic
// lifecycle builders. The family enum, rather than an arbitrary role string,
// preserves the authority boundary while keeping prepare/retire semantics in
// one implementation. `wamn_scenario_author` never appears here — it is the
// project plane's author role and is never granted control-database CONNECT.

/// Idempotently create or harden the stable control-author ACL role as NOLOGIN.
///
/// Create-or-*harden* under the shared `wamn_role_bootstrap` advisory lock, the
/// same shape as `wamn_ops` and `wamn_scenario_author`: a replay that finds the
/// role with a drifted attribute re-ALTERs it instead of reporting success.
/// Table and schema grants deliberately do not live here — the control
/// portable-store artifact owns them, applied as its owner after this role
/// exists.
pub fn ensure_control_author_acl_role_sql() -> String {
    ensure_workload_acl_role_sql(WorkloadRoleFamily::ControlAuthor)
}

/// Prepare one inactive scoped control-author generation for authenticated use.
///
/// `role` is the validated deterministic generation name from
/// [`crate::control_author_generation_role`]; `database` is the control
/// database. Create-then-`ALTER … LOGIN` in that order so a crash between the two
/// statements leaves an inert role and the same batch converges on replay.
/// Membership is exactly the one stable ACL role, and direct database authority
/// is solely `CONNECT` on the control database — never on a project database.
pub fn prepare_control_author_generation_sql(
    database: &str,
    role: &str,
    password: &str,
    expires_at: &str,
) -> String {
    prepare_workload_generation_sql(
        WorkloadRoleFamily::ControlAuthor,
        database,
        role,
        password,
        expires_at,
    )
}

/// Retire one old control-author generation after replacement use was verified.
///
/// Authority leaves before authentication does, so a session that survives the
/// first statement still cannot read or write. The caller commits this batch
/// before terminating sessions with
/// [`terminate_control_author_generation_sessions_sql`].
pub fn retire_control_author_generation_sql(database: &str, role: &str) -> String {
    retire_workload_generation_sql(WorkloadRoleFamily::ControlAuthor, database, role)
}

/// Terminate sessions only after control-author authority removal has committed.
pub fn terminate_control_author_generation_sessions_sql(role: &str) -> String {
    terminate_workload_generation_sessions_sql(role)
}

/// Record the owner-maintained login-identity-to-tenant mapping for one scope.
///
/// `$1` login identity, `$2` tenant, `$3` org, `$4` project, `$5` environment.
/// Tenant authority is database-authoritative: this row, not a caller-set
/// `app.tenant`, is what every author-accessed relation's restrictive policy
/// resolves through, so the statement is deliberately an owner statement with no
/// grant to the author. `DO UPDATE` is restricted to a row whose tenant already
/// matches, so a replay converges while a re-point to a different tenant returns
/// **zero rows** instead of widening one login's reach — the caller must read an
/// empty result as a refusal, not as a no-op.
pub fn upsert_control_author_tenant_mapping_sql() -> &'static str {
    "INSERT INTO wamn_authority.author_login_tenants \
       (login_identity, tenant_id, org_id, project_id, environment) \
     VALUES ($1, $2, $3, $4, $5) \
     ON CONFLICT (login_identity) DO UPDATE \
       SET org_id = EXCLUDED.org_id, \
           project_id = EXCLUDED.project_id, \
           environment = EXCLUDED.environment \
       WHERE author_login_tenants.tenant_id = EXCLUDED.tenant_id \
     RETURNING tenant_id"
}

// --- CDC capture provisioning (wamn-l5i9.9, D19 v3 §4) -----------------------
//
// The per-project-env CDC substrate: a REPLICATION role (the R8b credential
// tier above `wamn_app` query creds and the dispatch role), a publication over
// the app data schema, and a failover-enabled logical replication slot. The
// publication and the slot are DATABASE-BOUND — apply their SQL connected to
// the project-env database; the role is cluster-global. Pass the shared
// `cdc_object_name` (`wamn_cdc_<org>__<project>__<env>__<instance>`) as the role /
// publication / slot name.

/// Idempotently bootstrap a per-project-env **replication** role: `REPLICATION
/// LOGIN`, otherwise least-privilege (`NOSUPERUSER NOCREATEDB NOCREATEROLE
/// NOINHERIT NOBYPASSRLS` — `NOINHERIT` matches every other role this crate
/// mints; the role holds no memberships, so it is exactness, not a live
/// change). One role per project-env names the intended capture scope; it does
/// not limit a leaked credential to one registration. `REPLICATION` itself is
/// CLUSTER-WIDE in Postgres: any replication role can read any database's WAL
/// on that cluster. The accepted T3 boundary is compound: production HBA has
/// no physical-replication entry; PUBLIC CONNECT is revoked and this role gets
/// CONNECT only on its own database; ordinary DML remains denied; and each
/// reader is configured with its own slot and publication. The M1 gate proves
/// those production-shaped legs.
pub fn ensure_replication_role_sql(role: &str, password: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = {role_lit}) THEN \
             CREATE ROLE {role} LOGIN REPLICATION PASSWORD {pw} \
               NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS; \
           END IF; \
         END $$;",
        role = quote_ident(role),
        role_lit = quote_literal(role),
        pw = quote_literal(password),
    )
}

/// `CREATE SCHEMA IF NOT EXISTS "<schema>"` — the eager guard that makes the
/// CDC SQL order-robust: `FOR TABLES IN SCHEMA` auto-includes tables created
/// later, so the publication may be created BEFORE catalog-publish fills the
/// schema and still capture everything from the start. Catalog-publish's own
/// `CREATE SCHEMA IF NOT EXISTS` then no-ops.
pub fn ensure_schema_sql(schema: &str) -> String {
    format!("CREATE SCHEMA IF NOT EXISTS {}", quote_ident(schema))
}

/// Idempotently create the CDC publication over the project-env's app **data**
/// schema: `CREATE PUBLICATION <pub> FOR TABLES IN SCHEMA <schema>`, guarded by
/// a `pg_publication` probe (Postgres has no `CREATE PUBLICATION IF NOT
/// EXISTS`). `FOR TABLES IN SCHEMA` auto-includes tables created in the schema
/// later — the D19 v3 replacement for the retired per-table trigger emission.
/// Re-pointing an existing publication at a different schema is a manual
/// `ALTER PUBLICATION … SET TABLES IN SCHEMA` (the guard never rewrites).
/// Run connected to the project-env database.
pub fn create_publication_sql(publication: &str, schema: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_publication WHERE pubname = {pub_lit}) THEN \
             CREATE PUBLICATION {publication} FOR TABLES IN SCHEMA {schema}; \
           END IF; \
         END $$;",
        publication = quote_ident(publication),
        pub_lit = quote_literal(publication),
        schema = quote_ident(schema),
    )
}

/// Idempotently create the **failover-enabled** logical replication slot via
/// the SQL-function form: `pg_create_logical_replication_slot(<slot>,
/// 'pgoutput', temporary => false, twophase => false, failover => true)`
/// (PG17+ fifth argument) — a normal connection, no replication-protocol
/// syntax; the reader's `ensure_replication_slot` tolerates the existing slot
/// (same plugin/twophase/failover shape). Logical slots are DATABASE-BOUND:
/// run connected to the project-env database. WAL is pinned from creation
/// (capture starts at CDC-enable), bounded by the cluster's
/// `max_slot_wal_keep_size`.
pub fn create_failover_slot_sql(slot: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_replication_slots WHERE slot_name = {slot_lit}) THEN \
             PERFORM pg_create_logical_replication_slot({slot_lit}, 'pgoutput', false, false, true); \
           END IF; \
         END $$;",
        slot_lit = quote_literal(slot),
    )
}

/// Grant the replication role its read surface: `CONNECT` on the project-env
/// database, `USAGE` on the app data schema, and `SELECT` on **exactly one
/// table** — the decode-time entity map [`ensure_entity_map_sql`], which is the
/// reader's only table query against a project-env database.
///
/// It deliberately does NOT grant `SELECT ON ALL TABLES IN SCHEMA`. Logical
/// *decoding* reads WAL, not tables, and the shipped reader performs no
/// initial snapshot and no backfill (`StreamingMode::Off` is the pgoutput
/// in-progress-transaction option, not a snapshot), so the blanket grant bought
/// nothing and cost a plain-SQL read of every tenant table to the same
/// credential — with none of the gates (the `REPLICATION` attribute, the
/// walsender protocol, slot ownership) that guard the replication path.
///
/// The `REVOKE … ON ALL TABLES` is the **retroactive** half: environments
/// provisioned before this narrowing already executed the blanket grant, and a
/// narrower grant does not undo it. It precedes the entity-map grant because it
/// would otherwise strip it. Idempotent; run connected to the project-env
/// database AFTER the schema AND the entity map exist (`cdc_sql_bundle` emits
/// [`ensure_entity_map_sql`] first).
pub fn grant_replication_access_sql(database: &str, role: &str, schema: &str) -> String {
    let role = quote_ident(role);
    format!(
        "GRANT CONNECT ON DATABASE {db} TO {role}; \
         GRANT USAGE ON SCHEMA {schema} TO {role}; \
         REVOKE SELECT ON ALL TABLES IN SCHEMA {schema} FROM {role}; \
         GRANT SELECT ON {schema}.wamn_entities TO {role};",
        db = quote_ident(database),
        schema = quote_ident(schema),
    )
}

/// The decode-time entity map (wamn-l5i9.11, D19 v3 §4): `relation_oid` →
/// stable catalog entity id. **OID-keyed** so a reader resolving events is
/// timeless under catch-up — pg_class OIDs survive `ALTER TABLE RENAME`, so a
/// session decoding pre-rename backlog still resolves correctly, and a rename
/// only updates the informational `table_name`. Maintained by
/// `publish-catalog`/`migrate-catalog` in the same transaction as the DDL;
/// rows are upsert-only (a dropped entity's row keeps old-WAL decode
/// resolvable). No RLS: it holds no tenant data, and the CDC role's decode
/// stream sees every row of every table anyway.
pub fn ensure_entity_map_sql(schema: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {schema}.wamn_entities ( \
           relation_oid oid PRIMARY KEY, \
           entity_id text NOT NULL, \
           table_name text NOT NULL)",
        schema = quote_ident(schema),
    )
}

/// Upsert one entity's map row, resolving the table's CURRENT `pg_class` OID
/// server-side — run in the SAME transaction as the DDL that created/renamed
/// the table, so the row is atomic with the physical state. `$1` = entity id,
/// `$2` = physical table name. A table that does not exist (a catalog entity
/// whose floor was never applied) upserts nothing — the SELECT is empty.
pub fn upsert_entity_map_sql(schema: &str) -> String {
    format!(
        "INSERT INTO {schema}.wamn_entities (relation_oid, entity_id, table_name) \
         SELECT c.oid, $1, $2::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {schema_lit} AND c.relname = $2::text AND c.relkind = 'r' \
         ON CONFLICT (relation_oid) DO UPDATE \
           SET entity_id = EXCLUDED.entity_id, table_name = EXCLUDED.table_name",
        schema = quote_ident(schema),
        schema_lit = quote_literal(schema),
    )
}

/// `DROP PUBLICATION IF EXISTS "<publication>"` — teardown / gate only.
pub fn drop_publication_sql(publication: &str) -> String {
    format!("DROP PUBLICATION IF EXISTS {}", quote_ident(publication))
}

/// Drop the replication slot if it exists (teardown / gate only — dropping a
/// live slot severs the reader and releases the pinned WAL). Run connected to
/// the slot's database.
pub fn drop_replication_slot_sql(slot: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF EXISTS (SELECT FROM pg_replication_slots WHERE slot_name = {slot_lit}) THEN \
             PERFORM pg_drop_replication_slot({slot_lit}); \
           END IF; \
         END $$;",
        slot_lit = quote_literal(slot),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_app_role_is_least_privilege_and_idempotent() {
        let sql = ensure_app_role_sql("wamn_app");
        assert!(sql.contains("IF NOT EXISTS"), "idempotent guard");
        // `IF NOT EXISTS` alone is NOT idempotent under concurrency
        // (`wamn-0h0g.12.186`): roles are cluster-global, so two provisioners
        // that both find the role absent both issue CREATE and the loser gets
        // `duplicate_object`. The lock serializes the ones that take it; the
        // handler covers the ones that do not, and BOTH are required.
        assert!(sql.contains("pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'))"));
        assert!(sql.contains("EXCEPTION WHEN duplicate_object THEN NULL"));
        assert!(sql.contains("CREATE ROLE \"wamn_app\""));
        assert!(sql.contains("PASSWORD 'wamn_app'"));
        // No harden arm, and therefore no re-stamped password on a replay.
        assert!(!sql.contains("ALTER ROLE"));
        assert_eq!(sql.matches("PASSWORD").count(), 1);
        // Least privilege — every restrictive attribute is present.
        for attr in ["NOSUPERUSER", "NOCREATEDB", "NOCREATEROLE", "NOBYPASSRLS"] {
            assert!(sql.contains(attr), "missing {attr}");
        }
        // A password with a quote is escaped, not injected.
        assert!(ensure_app_role_sql("a'b").contains("PASSWORD 'a''b'"));
    }

    #[test]
    fn database_ddl_quotes_the_hyphenated_name() {
        assert_eq!(
            create_database_sql("acme-corp"),
            "CREATE DATABASE \"wamn-db-acme-corp\" OWNER \"wamn_db_owner\""
        );
        assert_eq!(
            drop_database_sql("acme-corp"),
            "DROP DATABASE IF EXISTS \"wamn-db-acme-corp\" WITH (FORCE)"
        );
    }

    #[test]
    fn named_database_ddl_targets_an_arbitrary_db_name_and_the_wrappers_delegate() {
        // The per-project-env path (wamn-q3n.7/.8) passes a full triple-derived name.
        assert_eq!(
            create_database_named_sql("wamn-db-acme--billing--dev"),
            "CREATE DATABASE \"wamn-db-acme--billing--dev\""
        );
        assert_eq!(
            drop_database_named_sql("wamn-db-acme--billing--dev"),
            "DROP DATABASE IF EXISTS \"wamn-db-acme--billing--dev\" WITH (FORCE)"
        );
        // The 2.3 create wrapper adds its stable title owner; the named helper
        // stays owner-neutral for substrate scaffolding.
        assert_eq!(
            create_database_sql("acme"),
            "CREATE DATABASE \"wamn-db-acme\" OWNER \"wamn_db_owner\""
        );
        assert_eq!(
            create_database_named_sql("wamn-db-acme"),
            "CREATE DATABASE \"wamn-db-acme\""
        );
        // The drop wrapper remains a direct delegation.
        assert_eq!(
            drop_database_sql("acme"),
            drop_database_named_sql("wamn-db-acme")
        );
    }

    #[test]
    fn grant_connect_revokes_public_then_grants_app_role() {
        let sql = grant_connect_sql("acme");
        // The REVOKE FROM PUBLIC must precede the GRANT (order is not load-bearing
        // for correctness, but both must be present — the isolation backstop).
        let revoke = sql
            .find("REVOKE CONNECT, TEMPORARY ON DATABASE \"wamn-db-acme\" FROM PUBLIC")
            .expect("revoke public");
        let grant = sql
            .find("GRANT CONNECT ON DATABASE \"wamn-db-acme\" TO \"wamn_app\"")
            .expect("grant app role");
        assert!(revoke < grant);
    }

    #[test]
    fn grant_connect_on_database_targets_an_arbitrary_db_name() {
        // The per-project-env path (wamn-q3n.7) passes a full triple-derived name.
        let sql = grant_connect_on_database_sql("wamn-db-acme--billing--dev");
        assert!(sql.contains(
            "REVOKE CONNECT, TEMPORARY ON DATABASE \"wamn-db-acme--billing--dev\" FROM PUBLIC"
        ));
        assert!(
            sql.contains(
                "GRANT CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" TO \"wamn_app\""
            )
        );
        // The project-taking 2.3 wrapper delegates to it with the derived name.
        assert_eq!(
            grant_connect_sql("acme"),
            grant_connect_on_database_sql("wamn-db-acme")
        );
    }

    #[test]
    fn the_dispatch_reader_is_a_connection_free_stable_acl_role() {
        // `wamn-0h0g.22.24`: the family HAD its own create-or-harden builder that
        // minted a LOGIN role with a documented default password, because the
        // dispatcher authenticated as the stable role itself. It now takes the
        // generic ACL-role builder like every other family, and the assertion
        // that matters is the NEGATIVE one — a builder that reintroduces LOGIN or
        // a password hands a cluster-global credential CONNECT that its
        // generations inherit into every database on the cluster.
        let sql = ensure_workload_acl_role_sql(WorkloadRoleFamily::DispatchReader);
        assert!(sql.contains("pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'))"));
        assert!(sql.contains("'wamn_dispatch_reader'"));
        assert!(sql.contains("CREATE ROLE %I NOLOGIN"));
        assert!(sql.contains("ALTER ROLE %I NOLOGIN PASSWORD NULL"));
        assert!(
            !sql.contains("PASSWORD '"),
            "the stable role carries no credential"
        );
        // The harden arm treats a role that CAN log in as drifted — the exact
        // negation the retired builder spelled the other way round.
        assert!(sql.contains("rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole"));
        assert!(sql.contains("OR rolpassword IS NOT NULL"));
        // This builder owns the role identity only — never a grant.
        for forbidden in ["ON SCHEMA", "ON TABLE", "CONNECT ON DATABASE"] {
            assert!(!sql.contains(forbidden), "role builder leaked a grant");
        }
    }

    #[test]
    fn dispatch_reader_read_surface_is_exactly_two_selects_and_narrows() {
        let sql = grant_dispatch_reader_read_surface_sql("wamn_run");
        // Every grant is preceded by a blanket REVOKE over the same scope, so a
        // widened environment CONVERGES BACK. Without these the batch could only
        // ever add authority, and an over-granted reader would survive forever.
        let revoke_tables = sql
            .find("REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA \"wamn_run\" FROM \"wamn_dispatch_reader\"")
            .expect("blanket table revoke");
        let revoke_schema = sql
            .find("REVOKE ALL PRIVILEGES ON SCHEMA \"wamn_run\" FROM \"wamn_dispatch_reader\"")
            .expect("blanket schema revoke");
        let grant_usage = sql
            .find("GRANT USAGE ON SCHEMA \"wamn_run\" TO \"wamn_dispatch_reader\"")
            .expect("schema usage");
        assert!(revoke_tables < grant_usage && revoke_schema < grant_usage);

        // Exactly the two relations the dispatcher reads, and exactly SELECT.
        for relation in DISPATCH_READER_RELATIONS {
            assert!(sql.contains(&format!(
                "GRANT SELECT ON \"wamn_run\".\"{relation}\" TO \"wamn_dispatch_reader\""
            )));
        }
        assert_eq!(sql.matches("GRANT SELECT ON").count(), 2);
        // No write verb, no EXECUTE, and no relation outside the pair. `runs` is
        // the pointed omission: the dispatcher joins the queue's budget clause to
        // `effect_attempts`, never to the run history.
        for forbidden in [
            "INSERT",
            "UPDATE",
            "DELETE",
            "TRUNCATE",
            "REFERENCES",
            "TRIGGER",
            "EXECUTE",
            "ALL TABLES IN SCHEMA \"wamn_run\" TO",
            "\"runs\"",
            "\"node_runs\"",
            "catalog",
        ] {
            assert!(
                !sql.contains(forbidden),
                "read surface gained {forbidden:?}"
            );
        }
        // The schema is an identifier position and is quoted, not interpolated.
        assert!(grant_dispatch_reader_read_surface_sql("we\"ird").contains("\"we\"\"ird\""));
    }

    #[test]
    fn management_admitter_surface_is_current_column_exact_and_convergent() {
        let sql = grant_management_admitter_surface_sql("wamn_run");
        assert!(sql.contains("'wamn_management_admitter'"));
        assert!(sql.contains("CREATE ROLE %I NOLOGIN"));
        let revoke = sql
            .find(
                "REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, \"wamn_run\" \
                 FROM \"wamn_management_admitter\"",
            )
            .expect("blanket table revoke");
        let usage = sql
            .find(
                "GRANT USAGE ON SCHEMA catalog, \"wamn_run\" \
                 TO \"wamn_management_admitter\"",
            )
            .expect("exact schema usage");
        assert!(revoke < usage);
        let column_revoke = sql
            .find("DO $management_column_acl$")
            .expect("explicit stale column-ACL revocation");
        assert!(revoke < column_revoke && column_revoke < usage);

        for relation in MANAGEMENT_ADMITTER_CATALOG_RELATIONS {
            assert!(sql.contains(&format!(
                "GRANT SELECT ON TABLE catalog.\"{relation}\" TO \
                 \"wamn_management_admitter\""
            )));
        }
        assert!(sql.contains(
            "GRANT SELECT ON TABLE \"wamn_run\".\"environment_policies\" \
             TO \"wamn_management_admitter\""
        ));
        assert!(sql.contains("GRANT SELECT (\"tenant_id\", \"run_id\", \"binding_world_json\""));
        // wamn-oici's probe arm, extended by wamn-0h0g.8.25. The consumers derive
        // their expectations from the same constant, so a widening flows through
        // them silently and they prove nothing about it. This names the
        // observation and evaluation columns literally in the emitted grant, so
        // dropping any of them fails HERE.
        assert!(
            sql.contains(
                "\"run_deadline_at\", \"status\", \"result_json\", \
                 \"caller_outcome_kind\", \"caller_outcome_json\", \
                 \"caller_http_status\", \"fail_kind\") \
                 ON TABLE \"wamn_run\".\"runs\""
            ),
            "the observation leg must read runs.status and runs.result_json, and the \
             evaluation leg the caller-outcome family plus runs.fail_kind"
        );
        assert!(sql.contains("GRANT INSERT (\"tenant_id\", \"run_id\", \"catalog_id\""));
        assert!(sql.contains(
            "GRANT SELECT (\"tenant_id\", \"run_id\"), INSERT (\"tenant_id\", \
             \"run_id\", \"available_at\", \"stream_seq\")"
        ));

        for forbidden in [
            "GRANT SELECT ON ALL TABLES",
            "GRANT INSERT ON TABLE catalog",
            "GRANT UPDATE",
            "GRANT DELETE",
            "GRANT TRUNCATE",
            "GRANT REFERENCES",
            "GRANT TRIGGER",
            "flow_id",
            "flow_version",
            "plan_hash",
            "execution_bundle_hash",
        ] {
            assert!(!sql.contains(forbidden), "surface gained {forbidden:?}");
        }
        assert!(grant_management_admitter_surface_sql("we\"ird").contains("\"we\"\"ird\""));
    }

    /// THE INSERT ABOVE IS DEAD WITHOUT THIS, and the ordinary sweep is the only
    /// place that says so before a cluster does (`wamn-0h0g.22.28`).
    ///
    /// `runs` carries the `runs_tkey` EXPRESSION INDEX over
    /// `wamn_authority.tenant_key(text)`, and PostgreSQL evaluates an index
    /// expression while inserting the row — so the column-exact `GRANT INSERT`
    /// this surface emits raises 42501 `permission denied for function
    /// tenant_key` without the one EXECUTE below. That defect shipped green for
    /// four beads because the only test that could see it is `#[ignore]`d.
    ///
    /// The surface must carry EXACTLY ONE `GRANT EXECUTE` and it must be that
    /// one: a second would be a widening this family's production statement does
    /// not need, and the schema-USAGE and `current_tenant_key` grants are
    /// measured NOT to be required.
    #[test]
    fn the_management_surface_grants_the_one_execute_its_tkey_index_needs() {
        let sql = grant_management_admitter_surface_sql("wamn_run");
        assert!(
            sql.contains(
                "GRANT EXECUTE ON FUNCTION \"wamn_authority\".tenant_key(text) \
                 TO \"wamn_management_admitter\""
            ),
            "the tkey expression index makes this EXECUTE load bearing for INSERT"
        );
        assert_eq!(
            sql.matches("GRANT EXECUTE").count(),
            1,
            "exactly one function grant, and it is the tenant-key derivation"
        );
        for forbidden in [
            "GRANT USAGE ON SCHEMA \"wamn_authority\"",
            "current_tenant_key",
        ] {
            assert!(
                !sql.contains(forbidden),
                "index-expression evaluation needs neither: surface gained {forbidden:?}"
            );
        }
    }

    /// The grant set MINUS the shared role bootstrap, so a pin can be exact.
    ///
    /// `ensure_workload_acl_role_sql` is the same text for all twelve families
    /// and is nobody's surface; splitting it off is what lets the two pins below
    /// be byte-equality against the thing that actually varies, instead of
    /// fragment `contains` checks that pass while a grant is deleted.
    fn surface_grants(family: WorkloadRoleFamily, sql: &str) -> String {
        sql.strip_prefix(&format!("{} ", ensure_workload_acl_role_sql(family)))
            .expect("a family surface opens with its own role bootstrap")
            .to_string()
    }

    /// THE CALLABLE-HTTP ADMITTER'S WHOLE SURFACE, PINNED AS A STRING
    /// (`wamn-0h0g.22.37`).
    ///
    /// The live gate that measures this from the server is `#[ignore]`-free but
    /// self-skipping, so it does not run in the ordinary sweep — and a mutant
    /// that dies only in a live gate ships green. This is generated SQL from a
    /// Rust builder, so byte-equality here is the only thing that catches the
    /// builder moving in a plain `cargo test`.
    #[test]
    fn the_http_admitter_surface_is_six_catalog_selects_and_nothing_else() {
        let sql = grant_http_admitter_surface_sql("wamn_run");
        assert_eq!(
            surface_grants(WorkloadRoleFamily::HttpAdmitter, &sql),
            "REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, \"wamn_run\" \
             FROM \"wamn_http_admitter\"; \
             REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, \"wamn_run\" \
             FROM \"wamn_http_admitter\"; \
             REVOKE ALL PRIVILEGES ON SCHEMA catalog, \"wamn_run\" \
             FROM \"wamn_http_admitter\"; \
             GRANT USAGE ON SCHEMA catalog TO \"wamn_http_admitter\"; \
             GRANT SELECT ON TABLE catalog.\"wirings\" TO \"wamn_http_admitter\"; \
             GRANT SELECT ON TABLE catalog.\"component_library\" TO \"wamn_http_admitter\"; \
             GRANT SELECT ON TABLE catalog.\"connection_requirements\" \
             TO \"wamn_http_admitter\"; \
             GRANT SELECT ON TABLE catalog.\"connection_bindings\" TO \"wamn_http_admitter\"; \
             GRANT SELECT ON TABLE catalog.\"connection_instances\" TO \"wamn_http_admitter\"; \
             GRANT SELECT ON TABLE catalog.\"connection_generations\" TO \"wamn_http_admitter\";",
            "the callable-HTTP admitter reads six catalog relations and holds \
             nothing on the run plane, no write of any grain, and no EXECUTE"
        );
        // The schema is an identifier position and is quoted, not interpolated.
        assert!(grant_http_admitter_surface_sql("we\"ird").contains("\"we\"\"ird\""));
    }

    /// THE EXECUTOR-PLATFORM SURFACE, PINNED AS A STRING (`wamn-0h0g.22.37`).
    ///
    /// The column lists are spelled out here rather than derived from the
    /// constants that produced them: deriving the expectation from the array
    /// under test is the tautology this branch has paid for before, and it would
    /// let a column be added to `EXECUTOR_PLATFORM_RUN_UPDATE_COLUMNS` with both
    /// sides moving together.
    #[test]
    fn the_executor_platform_surface_is_column_exact_on_every_write() {
        let sql = grant_executor_platform_surface_sql("wamn_run");
        assert_eq!(
            surface_grants(WorkloadRoleFamily::ExecutorPlatform, &sql),
            "REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA catalog, \"wamn_run\" \
             FROM \"wamn_executor_platform\"; \
             REVOKE ALL PRIVILEGES ON ALL ROUTINES IN SCHEMA catalog, \"wamn_run\" \
             FROM \"wamn_executor_platform\"; \
             REVOKE ALL PRIVILEGES ON SCHEMA catalog, \"wamn_run\" \
             FROM \"wamn_executor_platform\"; \
             GRANT USAGE ON SCHEMA catalog, \"wamn_run\" TO \"wamn_executor_platform\"; \
             GRANT SELECT ON TABLE catalog.\"wirings\" TO \"wamn_executor_platform\"; \
             GRANT SELECT ON TABLE catalog.\"component_library\" TO \"wamn_executor_platform\"; \
             GRANT SELECT ON TABLE catalog.\"connection_requirements\" \
             TO \"wamn_executor_platform\"; \
             GRANT SELECT ON TABLE catalog.\"connection_bindings\" \
             TO \"wamn_executor_platform\"; \
             GRANT SELECT ON TABLE catalog.\"connection_instances\" \
             TO \"wamn_executor_platform\"; \
             GRANT SELECT ON TABLE catalog.\"connection_generations\" \
             TO \"wamn_executor_platform\"; \
             GRANT SELECT ON TABLE catalog.\"catalog_heads\" TO \"wamn_executor_platform\"; \
             GRANT SELECT ON TABLE catalog.\"release_components\" TO \"wamn_executor_platform\"; \
             GRANT SELECT ON TABLE catalog.\"release_manifest_v2_snapshots\" \
             TO \"wamn_executor_platform\"; \
             GRANT SELECT ON TABLE catalog.\"wiring_activation\" TO \"wamn_executor_platform\"; \
             GRANT SELECT ON TABLE catalog.\"wiring_tombstones\" TO \"wamn_executor_platform\"; \
             GRANT SELECT ON TABLE \"wamn_run\".\"runs\" TO \"wamn_executor_platform\"; \
             GRANT UPDATE (\"status\", \"terminal_reason\", \"fail_kind\", \"result_json\", \
             \"state_json\", \"release_version\", \"manifest_digest\", \"caller_outcome_kind\", \
             \"caller_outcome_json\", \"caller_http_status\", \"caller_release_node_id\", \
             \"caller_outcome_hash\", \"caller_released_at\", \"updated_at\") \
             ON TABLE \"wamn_run\".\"runs\" TO \"wamn_executor_platform\"; \
             GRANT SELECT, DELETE ON TABLE \"wamn_run\".\"run_queue\" \
             TO \"wamn_executor_platform\"; \
             GRANT UPDATE (\"lease_owner\", \"lease_expires_at\", \"lease_generation\", \
             \"attempts\") ON TABLE \"wamn_run\".\"run_queue\" TO \"wamn_executor_platform\"; \
             GRANT SELECT ON TABLE \"wamn_run\".\"effect_attempts\" \
             TO \"wamn_executor_platform\"; \
             GRANT EXECUTE ON FUNCTION \
             \"wamn_run\".\"require_executor_platform_authority\"() \
             TO \"wamn_executor_platform\"; \
             GRANT EXECUTE ON FUNCTION \"wamn_authority\".tenant_key(text) \
             TO \"wamn_executor_platform\";",
            "the executor-platform grant set moved: it is TABLE SELECT where a \
             fence reads r.*/q.*, COLUMN UPDATE everywhere it writes, DELETE only \
             on the queue, and exactly two function grants"
        );
        // The admission pins, the frozen wiring identity and the authoritative
        // input are UNREACHABLE by this family. Named literally, because they are
        // absent from a constant and an absence proves nothing by itself.
        for forbidden in [
            "GRANT INSERT",
            "GRANT TRUNCATE",
            "GRANT REFERENCES",
            "GRANT TRIGGER",
            "GRANT UPDATE ON TABLE",
            "GRANT DELETE ON TABLE \"wamn_run\".\"runs\"",
            "\"input_json\"",
            "\"binding_world_json\"",
            "\"durability_class\"",
            "\"wiring_hash\"",
            "\"available_at\"",
            "\"max_attempts\"",
            "\"stream_seq\"",
            "environment_policies",
            "current_tenant_key",
            "GRANT USAGE ON SCHEMA \"wamn_authority\"",
        ] {
            assert!(!sql.contains(forbidden), "surface gained {forbidden:?}");
        }
        assert!(grant_executor_platform_surface_sql("we\"ird").contains("\"we\"\"ird\""));
    }

    #[test]
    fn management_admitter_prepare_uses_the_generic_generation_lifecycle() {
        let role = "management-generation-a";
        let sql = prepare_workload_generation_sql(
            WorkloadRoleFamily::ManagementAdmitter,
            "project-db",
            role,
            "secret",
            "2026-09-15T12:00:00Z",
        );
        assert!(sql.contains(&grant_management_admitter_surface_sql("wamn_run")));
        assert!(sql.contains(&format!(
            "GRANT \"wamn_management_admitter\" TO \"{role}\" \
             WITH ADMIN FALSE, INHERIT TRUE, SET FALSE"
        )));
        assert!(sql.contains(&format!(
            "GRANT CONNECT ON DATABASE \"project-db\" TO \"{role}\""
        )));
        assert_eq!(sql.matches("LOGIN PASSWORD 'secret'").count(), 1);
        assert!(!sql.contains("CREATE SECRET"));
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
             GRANT SELECT ON TABLE \"registry\".\"event_readers\" TO \"wamn_registry_reader\";"
        );
    }

    /// THE DISJOINTNESS GUARD, from the registry side (`wamn-0h0g.12.116`).
    ///
    /// One SELECT and one relation, and not one syllable of `identity`
    /// downstream of the revocation. Widening this role onto the identity plane
    /// — the one edit that would collapse the two disjoint grant sets into a
    /// union — fails here.
    #[test]
    fn the_registry_reader_is_never_granted_anything_on_the_identity_plane() {
        let sql = grant_registry_reader_surface_sql();
        assert_eq!(sql.matches("GRANT SELECT").count(), 1);
        assert_eq!(sql.matches("GRANT USAGE ON SCHEMA").count(), 1);
        assert_eq!(REGISTRY_READER_RELATIONS.len(), 1);
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
    fn the_identity_reader_grants_exactly_three_selects_and_revokes_the_whole_plane() {
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
             GRANT SELECT ON TABLE \"identity\".\"project_roles\" TO \"wamn_identity_reader\";"
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
            IDENTITY_READER_RELATIONS.len(),
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
    fn the_dispatch_reader_connect_builder_revokes_and_never_grants() {
        let sql = revoke_dispatch_reader_connect_sql("wamn-db-acme--billing--dev");
        assert_eq!(
            sql,
            "REVOKE CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" \
             FROM \"wamn_dispatch_reader\";"
        );
        // `wamn-0h0g.22.24`: the direction is the fix. A `GRANT` here is
        // cluster-global reach, because every dispatch-reader generation is a
        // member of this role WITH INHERIT TRUE.
        assert!(!sql.contains("GRANT"));
        // It touches ONE principal, so one edit cannot move both.
        assert!(!sql.contains(APP_ROLE));
        assert!(
            !grant_connect_on_database_sql("wamn-db-acme--billing--dev")
                .contains(DISPATCH_READER_ROLE)
        );
        // The generation is where CONNECT lives now, and only for its own
        // database.
        let generation = prepare_workload_generation_sql(
            WorkloadRoleFamily::DispatchReader,
            "wamn-db-acme--billing--dev",
            "wamn_dispatch_reader_0123456789abcdef0123456789abcdef01234567_a",
            "pw",
            "2030-01-01T00:00:00Z",
        );
        assert!(generation.contains(
            "GRANT CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" \
             TO \"wamn_dispatch_reader_0123456789abcdef0123456789abcdef01234567_a\";"
        ));
        assert_eq!(generation.matches("GRANT CONNECT ON DATABASE").count(), 1);
    }

    #[test]
    fn writer_acl_roles_are_stable_nologin_and_own_no_grants_here() {
        let sql = ensure_effect_writer_acl_role_sql();
        assert!(sql.contains("'wamn_effect_writer'"));
        assert!(!sql.contains("'wamn_run_projection_writer'"));
        assert!(sql.contains("CREATE ROLE %I NOLOGIN"));
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
        for forbidden in ["CONNECT ON DATABASE", "ON SCHEMA", "ON TABLE"] {
            assert!(
                !sql.contains(forbidden),
                "provisioning stole schema-control grant ownership"
            );
        }
    }

    #[test]
    fn generation_prepare_has_only_login_membership_and_project_connect() {
        let role = "wamn_effect_writer_1111111111111111111111111111111111111111_a";
        let sql = prepare_effect_writer_generation_sql(
            "wamn-db-acme--billing--dev",
            role,
            "a'b",
            "2026-09-01T00:00:00Z",
        );
        assert!(sql.contains(&format!("CREATE ROLE \"{role}\" NOLOGIN")));
        assert!(sql.contains(&format!(
            "ALTER ROLE \"{role}\" LOGIN PASSWORD 'a''b' VALID UNTIL '2026-09-01T00:00:00Z'"
        )));
        assert!(sql.contains(&format!("GRANT \"wamn_effect_writer\" TO \"{role}\"")));
        assert!(sql.contains(&format!(
            "REVOKE \"wamn_run_projection_writer\" FROM \"{role}\""
        )));
        assert!(sql.contains("WITH ADMIN FALSE, INHERIT TRUE, SET FALSE"));
        assert!(sql.contains(&format!(
            "GRANT CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" TO \"{role}\""
        )));
        for forbidden in [
            "SUPERUSER",
            "CREATEDB",
            "CREATEROLE",
            "REPLICATION",
            "BYPASSRLS",
        ] {
            assert!(sql.contains(&format!("NO{forbidden}")));
        }
        assert!(!sql.contains("GRANT USAGE"));
        assert!(!sql.contains("GRANT SELECT"));
        assert!(!sql.contains("GRANT INSERT"));
    }

    #[test]
    fn generation_retire_commits_authority_removal_before_session_termination() {
        let role = "wamn_effect_writer_1111111111111111111111111111111111111111_a";
        let sql = retire_effect_writer_generation_sql("wamn-db-acme--billing--dev", role);
        let membership = sql.find("REVOKE \"wamn_effect_writer\"").unwrap();
        let connect = sql.find("REVOKE CONNECT ON DATABASE").unwrap();
        let no_login = sql.find("NOLOGIN PASSWORD NULL").unwrap();
        assert!(membership < connect && connect < no_login);
        assert!(!sql.contains("pg_terminate_backend"));

        let terminate = terminate_effect_writer_generation_sessions_sql(role);
        assert!(terminate.contains(&format!("WHERE usename = '{role}'")));
        assert!(terminate.contains("pid <> pg_backend_pid()"));
        for authority_change in ["REVOKE ", "ALTER ROLE"] {
            assert!(!terminate.contains(authority_change));
        }
    }

    /// wamn-0h0g.8.18: the stable control-author role is host-only, hardens on
    /// replay, and never becomes a member of another plane's role.
    #[test]
    fn control_author_acl_role_is_nologin_and_hardens_on_replay() {
        let sql = ensure_control_author_acl_role_sql();
        assert!(sql.contains("pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'))"));
        assert!(sql.contains("'wamn_control_author'"));
        assert!(sql.contains("CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE"));
        // A replay that finds a drifted attribute must repair it, not succeed.
        assert!(sql.contains("ELSIF EXISTS"));
        assert!(sql.contains("ALTER ROLE %I NOLOGIN PASSWORD NULL NOSUPERUSER NOCREATEDB"));
        for other_plane in [
            "wamn_scenario_author",
            "wamn_effect_writer",
            "wamn_run_projection_writer",
            "wamn_app",
        ] {
            assert!(
                !sql.contains(other_plane),
                "control author touched {other_plane}"
            );
        }
    }

    /// The prepared generation may authenticate and connect to exactly the
    /// control database, inheriting exactly the one stable ACL role.
    #[test]
    fn control_author_prepare_has_only_control_connect_and_one_membership() {
        let role = "wamn_control_author_2222222222222222222222222222222222222222_b";
        let sql = prepare_control_author_generation_sql(
            "wamn-system",
            role,
            "0123456789abcdef",
            "2026-09-15T12:00:00Z",
        );
        assert!(sql.contains(&ensure_control_author_acl_role_sql()));
        // NOLOGIN create precedes the LOGIN flip, so a crash between them leaves
        // an inert role rather than a passwordless login.
        let created = sql.find("CREATE ROLE \"wamn_control_author_2").unwrap();
        let login = sql.find("LOGIN PASSWORD '0123456789abcdef'").unwrap();
        assert!(created < login);
        assert!(sql[created..login].contains("NOLOGIN"));
        assert!(sql.contains("VALID UNTIL '2026-09-15T12:00:00Z'"));
        assert!(sql.contains(&format!(
            "GRANT \"wamn_control_author\" TO \"{role}\" \
             WITH ADMIN FALSE, INHERIT TRUE, SET FALSE;"
        )));
        assert_eq!(sql.matches("GRANT CONNECT ON DATABASE").count(), 1);
        assert!(sql.contains("GRANT CONNECT ON DATABASE \"wamn-system\""));
        // Never the project plane's author role, and never another plane's ACL.
        for forbidden in [
            "wamn_scenario_author",
            "wamn_effect_writer",
            "wamn_run_projection_writer",
            "SUPERUSER TO",
            "WITH ADMIN TRUE",
            "WITH GRANT OPTION",
        ] {
            assert!(!sql.contains(forbidden), "prepare granted {forbidden}");
        }
    }

    /// Retirement removes authority before authentication and never terminates a
    /// session in the same batch; the terminate builder changes no authority.
    #[test]
    fn control_author_retire_commits_authority_removal_before_termination() {
        let role = "wamn_control_author_3333333333333333333333333333333333333333_a";
        let sql = retire_control_author_generation_sql("wamn-system", role);
        let membership = sql.find("REVOKE \"wamn_control_author\"").unwrap();
        let connect = sql.find("REVOKE CONNECT ON DATABASE").unwrap();
        let no_login = sql.find("NOLOGIN PASSWORD NULL").unwrap();
        assert!(membership < connect && connect < no_login);
        assert!(sql.contains("VALID UNTIL 'epoch'"));
        assert!(!sql.contains("pg_terminate_backend"));
        assert!(
            !sql.contains("DROP ROLE"),
            "slots are reused, never dropped"
        );

        let terminate = terminate_control_author_generation_sessions_sql(role);
        assert!(terminate.contains(&format!("WHERE usename = '{role}'")));
        assert!(terminate.contains("pid <> pg_backend_pid()"));
        for authority_change in ["REVOKE ", "GRANT ", "ALTER ROLE"] {
            assert!(!terminate.contains(authority_change));
        }
    }

    /// The mapping is owner-maintained and cannot be re-pointed to another
    /// tenant: a converging replay updates the scope columns, a tenant change
    /// matches no row and therefore returns nothing.
    #[test]
    fn tenant_mapping_upsert_converges_but_never_repoints_a_tenant() {
        let sql = upsert_control_author_tenant_mapping_sql();
        assert!(sql.starts_with("INSERT INTO wamn_authority.author_login_tenants"));
        for parameter in ["$1", "$2", "$3", "$4", "$5"] {
            assert!(sql.contains(parameter), "mapping upsert drops {parameter}");
        }
        assert_eq!(sql.matches('$').count(), 5);
        assert!(sql.contains("ON CONFLICT (login_identity) DO UPDATE"));
        assert!(
            sql.contains("WHERE author_login_tenants.tenant_id = EXCLUDED.tenant_id"),
            "a mapping row's tenant must not be re-pointed: {sql}"
        );
        // The tenant column is never in the SET list, so even a matching row
        // cannot have its tenant rewritten.
        let set_list = sql.split("DO UPDATE").nth(1).expect("an upsert body");
        let set_list = set_list.split("WHERE").next().expect("a SET clause");
        assert!(!set_list.contains("tenant_id"), "{set_list}");
        assert!(sql.contains("RETURNING tenant_id"));
        // Owner-maintained: the statement grants nothing and names no author role.
        for forbidden in ["GRANT", "wamn_control_author", "app.tenant"] {
            assert!(!sql.contains(forbidden), "mapping upsert names {forbidden}");
        }
    }

    #[test]
    fn generation_state_probe_is_read_only_and_parameterized() {
        let sql = effect_writer_generation_state_sql();
        assert!(sql.starts_with("SELECT"));
        assert!(sql.contains("rolcanlogin"));
        assert!(sql.contains("FROM pg_catalog.pg_authid r"));
        assert!(sql.contains("r.rolpassword IS NOT NULL AS password_set"));
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
    fn generation_acl_inventory_covers_cluster_and_database_object_classes() {
        let sql = role_database_acl_inventory_sql();
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
        assert!(revoke_public_connect_floor_sql().contains("WHERE datallowconn"));
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
            effect_writer_scope_lock_sql(),
            "SELECT pg_advisory_lock(hashtextextended($1::text, 0))"
        );
    }

    #[test]
    fn replication_role_is_replication_login_and_otherwise_least_privilege() {
        let sql = ensure_replication_role_sql("wamn_cdc_acme__billing__dev", "s3cr3t");
        assert!(sql.contains("IF NOT EXISTS"), "idempotent guard");
        assert!(sql.contains("CREATE ROLE \"wamn_cdc_acme__billing__dev\" LOGIN REPLICATION"));
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
        // per-table form and never FOR ALL TABLES (which would leak app_system
        // and any other schema into the stream).
        assert!(sql.contains(
            "CREATE PUBLICATION \"wamn_cdc_acme__billing__dev\" FOR TABLES IN SCHEMA \"app\""
        ));
        assert!(!sql.contains("FOR ALL TABLES"));
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

    /// The CDC role reads WAL, not tables. Its ONLY table read is the entity
    /// map, so that is its only `SELECT` — and the retroactive `REVOKE` must
    /// precede the entity-map grant or it would strip it.
    #[test]
    fn replication_grants_cover_connect_usage_and_only_the_entity_map() {
        let sql = grant_replication_access_sql("wamn-db-acme--billing--dev", "wamn_cdc_x", "app");
        assert!(sql.contains(
            "GRANT CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" TO \"wamn_cdc_x\""
        ));
        assert!(sql.contains("GRANT USAGE ON SCHEMA \"app\" TO \"wamn_cdc_x\""));
        assert!(sql.contains("GRANT SELECT ON \"app\".wamn_entities TO \"wamn_cdc_x\""));
        // Never the blanket read of every tenant table.
        assert!(!sql.contains("GRANT SELECT ON ALL TABLES"));
        // The retroactive half, ordered before the narrow grant it would strip.
        let revoke = sql
            .find("REVOKE SELECT ON ALL TABLES IN SCHEMA \"app\" FROM \"wamn_cdc_x\"")
            .expect("retroactive revoke for already-provisioned environments");
        let grant = sql
            .find("GRANT SELECT ON \"app\".wamn_entities")
            .expect("entity-map grant");
        assert!(
            revoke < grant,
            "the revoke would strip the entity-map grant"
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
            "ALTER ROLE wamn_db_owner NOLOGIN NOSUPERUSER NOCREATEDB \
             NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS"
        ));
        // Owner-only by construction: no grants, no memberships, no generation
        // pair — a role nobody logs in as has nothing to rotate.
        for forbidden in ["GRANT ", "PASSWORD", "VALID UNTIL", "LOGIN PASSWORD"] {
            assert!(!sql.contains(forbidden), "db owner grew {forbidden}");
        }
        assert!(!sql.contains(APP_ROLE), "db owner is not wamn_app");
        assert!(!sql.contains("postgres"), "db owner is not a superuser");
    }

    /// The convergence half of the ownership migration: an `ALTER`, because a
    /// `REVOKE` cannot express ownership.
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

    /// The entity-map drift guard (wamn-l5i9.11): the PINNED SQL is the
    /// load-bearing contract — the reader's OID lookup, the same-transaction
    /// upsert, and the OID-keyed rename-proofing all ride these exact strings.
    #[test]
    fn entity_map_is_oid_keyed_and_upserted_from_pg_class() {
        assert_eq!(
            ensure_entity_map_sql("app"),
            "CREATE TABLE IF NOT EXISTS \"app\".wamn_entities ( \
               relation_oid oid PRIMARY KEY, \
               entity_id text NOT NULL, \
               table_name text NOT NULL)"
        );
        let upsert = upsert_entity_map_sql("app");
        // The OID is resolved server-side from pg_class IN the DDL transaction
        // (ordinary tables only), keyed for conflict on relation_oid — a
        // rename re-upserts the SAME row (new table_name, same entity/OID).
        assert!(
            upsert.contains(
                "INSERT INTO \"app\".wamn_entities (relation_oid, entity_id, table_name)"
            )
        );
        // `$2::text` in BOTH the projection and the WHERE — a bare `$2` would
        // be deduced `name` at `c.relname = $2` and `text` at the column,
        // which tokio_postgres rejects ("inconsistent types deduced").
        assert!(upsert.contains("SELECT c.oid, $1, $2::text FROM pg_class c"));
        assert!(
            upsert.contains("WHERE n.nspname = 'app' AND c.relname = $2::text AND c.relkind = 'r'")
        );
        assert!(upsert.contains("ON CONFLICT (relation_oid) DO UPDATE"));
        assert!(
            upsert.contains("SET entity_id = EXCLUDED.entity_id, table_name = EXCLUDED.table_name")
        );
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

    #[test]
    fn database_exists_is_parameterized() {
        assert_eq!(
            database_exists_sql(),
            "SELECT EXISTS (SELECT FROM pg_database WHERE datname = $1)"
        );
    }

    /// The escapers are inlined to keep the prod dep closure at `serde_json`, but
    /// they MUST stay byte-identical to the canonical `wamn_schema_compiler::sql::*` that back
    /// the injection-safety argument (a slug cannot contain a `"`, so the derived
    /// database/role DDL is safe). Assert over adversarial inputs plus an
    /// exhaustive single-ASCII-char sweep so any future divergence in either copy
    /// fails here. (`wamn-schema-compiler` is a dev-dependency, so this costs the prod build
    /// nothing.)
    #[test]
    fn inlined_escapers_match_canonical_wamn_schema_compiler() {
        let mut cases: Vec<String> = vec![
            "".into(),
            "a".into(),
            "plain_ident".into(),
            "a\"b".into(),
            "\"\"".into(),
            "a'b".into(),
            "''".into(),
            "a\"'b".into(),
            "back\\slash".into(),
            "tab\there".into(),
            "new\nline".into(),
            "nul\0byte".into(),
            "münz".into(),
            "wamn-db-acme--billing--dev".into(),
            "'; DROP TABLE x; --".into(),
        ];
        for c in 0u8..=0x7f {
            cases.push(format!("x{}y", c as char));
        }
        for s in &cases {
            assert_eq!(
                quote_ident(s),
                wamn_pg_core::quote_ident(s),
                "quote_ident drift on {s:?}"
            );
            assert_eq!(
                quote_literal(s),
                wamn_pg_core::quote_literal(s),
                "quote_literal drift on {s:?}"
            );
        }
    }
}

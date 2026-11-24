//! Pure SQL text builders for the T1 control-plane registry (wamn-q3n.6).
//!
//! Registry SQL lives with the registry model (SR2: the single source, like
//! `wamn-run-state` owns the `runs` SQL), drift-guarded against the storage DDL
//! in `deploy/sql/system-schema.sql`. Values travel as `$n` params; the driver (the
//! `provision-org` subcommand) holds the `wamn_system` connection and executes
//! the statement as the registry owner.

/// The `registry.environment_projects` column list, in the order both reads return and a
/// row-mapper reads by index. `org` is the caller's key in both reads, so it is
/// not returned; the remaining columns are exactly what a
/// project deployment needs (the triple's `org` comes from the
/// parameter). `instance_suffix` is here because nothing else can supply it — it
/// is the one part of a derived physical name that the triple does not carry
/// (wamn-0h0g.15.89).
const PROJECT_ENV_COLUMNS: &str = "env, project, secret_name, secret_namespace, instance_suffix";

/// Read ONE provisioned project-env by its identity triple — the read a consumer
/// that derives physical names needs, since a triple alone cannot yield the
/// environment's `instance_suffix` and only the provisioner sees it come back
/// when registration writes it. Params: `$1` org, `$2` project, `$3` env.
/// Columns: `PROJECT_ENV_COLUMNS`.
pub fn select_project_env_sql() -> String {
    format!(
        "SELECT {PROJECT_ENV_COLUMNS} FROM registry.environment_projects \
         WHERE org = $1 AND project = $2 AND env = $3"
    )
}

// --- event readers (wamn-l5i9.9, D19 v3) ------------------------------------

/// Record the applied CDC reader configuration and keep its original creation time.
/// Parameters follow the insert column order.
pub fn upsert_event_reader_sql() -> &'static str {
    "INSERT INTO registry.event_readers \
       (org, project, env, publication, schema, slot, stream, \
        replication_secret_name, replication_secret_namespace) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
     ON CONFLICT (org, env, project) DO UPDATE SET \
       publication = EXCLUDED.publication, schema = EXCLUDED.schema, \
       slot = EXCLUDED.slot, stream = EXCLUDED.stream, \
       replication_secret_name = EXCLUDED.replication_secret_name, \
       replication_secret_namespace = EXCLUDED.replication_secret_namespace, \
       updated_at = now()"
}

/// Read what `recover-capture-gap` needs of one CDC reader registration
/// (`wamn-59z6`). Params: `$1` org, `$2` project, `$3` env. Columns: `slot`,
/// `stream`, and `created_at`, the start of a gap on a stream with no CDC event.
pub fn select_event_reader_origin_sql() -> &'static str {
    "SELECT slot, stream, created_at \
     FROM registry.event_readers \
     WHERE org = $1 AND project = $2 AND env = $3"
}

/// Record one capture gap (`wamn-59z6`). Params: `$1` org, `$2` project, `$3`
/// env, `$4` the lost slot, `$5` start LSN as text (nullable), `$6` start time,
/// `$7` reason, `$8` end LSN as text. Returns `created_at`.
pub fn insert_capture_gap_sql() -> &'static str {
    "INSERT INTO registry.capture_gap \
       (org, project, env, slot, start_lsn, start_at, reason, end_lsn) \
     VALUES ($1, $2, $3, $4, $5::text::pg_lsn, $6, $7, $8::text::pg_lsn) \
     RETURNING created_at"
}

/// Lock the newest capture gap of one registration (`wamn-59z6`). Params: `$1`
/// org, `$2` project, `$3` env. Columns: `created_at`, `slot`, and `resync_at`.
pub fn lock_newest_capture_gap_sql() -> &'static str {
    "SELECT created_at, slot, resync_at \
     FROM registry.capture_gap \
     WHERE org = $1 AND project = $2 AND env = $3 \
     ORDER BY created_at DESC LIMIT 1 FOR UPDATE"
}

/// Close one capture gap (`wamn-59z6`). Params: `$1` org, `$2` project, `$3`
/// env, `$4` the row's `created_at`. Returns `resync_at`.
pub fn close_capture_gap_sql() -> &'static str {
    "UPDATE registry.capture_gap SET resync_at = now() \
     WHERE org = $1 AND project = $2 AND env = $3 AND created_at = $4 \
     RETURNING resync_at"
}

/// Lifecycle state belongs to the environment.
pub const ENVIRONMENT_DISPOSABLE: &str =
    "SELECT disposable FROM registry.environments WHERE org=$1 AND env=$2";

/// crates/environment/src/org.rs::ENVIRONMENT_STATUS_SQL registry statement.
pub const IDENTITY_PLATFORM_SRC_ORG_ENVIRONMENT_STATUS_SQL: &str = "UPDATE registry.environments SET status = $4 \
    WHERE org = $1 AND env = ANY($3) AND EXISTS (SELECT FROM registry.environment_projects p WHERE p.org=$1 AND p.env=registry.environments.env AND p.project=$2)";

/// crates/environment/src/org.rs::ORG_ENVIRONMENTS_SQL registry statement.
pub const IDENTITY_PLATFORM_SRC_ORG_ORG_ENVIRONMENTS_SQL: &str = "SELECT project, env FROM registry.environment_projects \
    WHERE org = $1 AND ($2::text IS NULL OR project = $2) ORDER BY project, env";

/// crates/environment/src/org.rs::CREATE_PROJECT_SQL registry statement.
pub const IDENTITY_PLATFORM_SRC_ORG_CREATE_PROJECT_SQL: &str =
    "INSERT INTO registry.projects (org, id) VALUES ($1, $2) ON CONFLICT (org, id) DO NOTHING";

/// crates/dev/src/pat_secrets_tests.rs::auth_drops_mint_pat_records_the_committed_project_role_drop registry statement.
pub const CONTROL_DEV_SRC_PAT_SECRETS_TESTS_AUTH_DROPS_MINT_PAT_RECORDS_THE_COMMITTED_PROJECT_ROLE_DROP: &str = "INSERT INTO registry.orgs (id) VALUES ('org'); INSERT INTO registry.projects (org, id) VALUES ('org', 'project')";

/// crates/dev/src/dev/target_database.rs::recorded_disposable registry statement.
pub const CONTROL_DEV_SRC_DEV_TARGET_DATABASE_RECORDED_DISPOSABLE: &str = "SELECT disposable FROM registry.environment_projects JOIN registry.environments USING (org,env) WHERE org=$1 AND project=$2 AND env=$3";

/// crates/provision/src/sql.rs::control_surface_grants_sql registry statement.
pub const CONTROL_PROVISION_SRC_SQL_CONTROL_SURFACE_GRANTS_SQL: &str = " GRANT EXECUTE ON FUNCTION registry.login_org(), registry.login_principal(uuid), identity.principal_in_login_org_only(uuid), identity.is_project_admin(uuid,text,text), identity.has_project_access(uuid,text,text,text) TO wamn_control;";

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
pub const UPDATE_PLATFORM_META_SQL: &str =
    "UPDATE registry.meta SET platform_domain = $1, max_projects = $2 \
     WHERE (platform_domain, max_projects) IS DISTINCT FROM ($1, $2) \
     RETURNING old.max_projects IS NULL";

/// crates/environment/src/environment/analyse.rs::read_row registry statement.
pub const CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_ANALYSE_READ_ROW: &str = "SELECT instance_suffix, disposable FROM registry.environment_projects JOIN registry.environments USING (org,env) \
              WHERE org = $1 AND project = $2 AND env = $3";

/// crates/environment/src/environment/status.rs::stranded_runs registry statement.
pub const CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_STATUS_STRANDED_RUNS: &str =
    "SELECT 1 FROM registry.environment_projects WHERE org = $1 AND project = $2 AND env = $3";

/// crates/environment/src/environment/delete.rs::delete_locked registry statement.
pub const CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_DELETE_DELETE_LOCKED: &str = "SELECT instance_suffix, drain_bound_seconds FROM registry.environment_projects JOIN registry.environments USING (org,env) \
              WHERE org = $1 AND project = $2 AND env = $3";

/// crates/environment/src/environment/delete/rows.rs::delete_registry_rows registry statement.
pub const CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_DELETE_ROWS_DELETE_REGISTRY_ROWS: &str =
    "DELETE FROM registry.environment_projects WHERE org = $1 AND project = $2 AND env = $3";

/// Registered orgs whose resources belong to the supplied namespace.
pub const INSTALL_ORGS: &str = "SELECT id FROM registry.orgs WHERE id=$1 ORDER BY id";

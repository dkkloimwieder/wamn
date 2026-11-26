//! Registry statements for environment lifecycle operations.

/// Record the applied CDC reader configuration and keep its original creation time.
/// Parameters follow the insert column order.
pub fn upsert_event_reader_sql() -> &'static str {
    "INSERT INTO registry.event_reader \
       (org, project, env, publication, schema, slot, stream, \
        replication_secret_name, replication_secret_namespace) \
     SELECT p.org, p.project, p.env, $4, $5, $6, $7, $8, $9 \
     FROM registry.project_env p WHERE p.org = $1 AND p.project = $2 AND p.env = $3 AND p.active \
     ON CONFLICT (org, env, project) DO UPDATE SET \
       publication = EXCLUDED.publication, schema = EXCLUDED.schema, \
       slot = EXCLUDED.slot, stream = EXCLUDED.stream, \
       replication_secret_name = EXCLUDED.replication_secret_name, \
       replication_secret_namespace = EXCLUDED.replication_secret_namespace, \
       updated_at = now()"
}

/// Read what `recover-capture-gap` needs of one CDC reader registration
/// (`wamn-59z6`). Params: `$1` org, `$2` project, `$3` env. Returns the capture position,
/// `stream`, and `created_at`, the start of a gap on a stream with no CDC event.
pub fn select_event_reader_origin_sql() -> &'static str {
    "SELECT r.slot, r.stream, r.created_at \
     FROM registry.event_reader r JOIN registry.project_env p USING (org, env, project) \
     WHERE r.org = $1 AND r.project = $2 AND r.env = $3 AND p.active"
}

/// Record one capture gap (`wamn-59z6`). Params: `$1` org, `$2` project, `$3`
/// env, `$4` the lost capture position, `$5` start LSN as text (nullable), `$6` start time,
/// `$7` reason, `$8` end LSN as text. Returns `created_at`.
pub fn insert_capture_gap_sql() -> &'static str {
    "INSERT INTO registry.capture_gap \
       (org, project, env, slot, start_lsn, start_at, reason, end_lsn) \
     VALUES ($1, $2, $3, $4, $5::text::pg_lsn, $6, $7, $8::text::pg_lsn) \
     RETURNING created_at"
}

/// Lock the newest capture gap of one registration (`wamn-59z6`). Params: `$1`
/// org, `$2` project, `$3` env. Returns creation time, capture position, and resynchronization time.
pub fn lock_newest_capture_gap_sql() -> &'static str {
    "SELECT g.created_at, g.slot, g.resync_at \
     FROM registry.capture_gap g JOIN registry.project_env p USING (org, env, project) \
     WHERE g.org = $1 AND g.project = $2 AND g.env = $3 AND p.active \
     ORDER BY g.created_at DESC LIMIT 1 FOR UPDATE OF g"
}

/// Close one capture gap (`wamn-59z6`). Params: `$1` org, `$2` project, `$3`
/// env, `$4` the row's `created_at`. Returns `resync_at`.
pub fn close_capture_gap_sql() -> &'static str {
    "UPDATE registry.capture_gap SET resync_at = now() \
     WHERE org = $1 AND project = $2 AND env = $3 AND created_at = $4 \
     RETURNING resync_at"
}

/// Registry statement used by [`crate::env_org::set_env_status`].
pub const IDENTITY_PLATFORM_SRC_ORG_ENV_STATUS_SQL: &str = "UPDATE registry.env SET active = $4 \
    WHERE org = $1 AND env = ANY($3) AND EXISTS (SELECT FROM registry.project_env p WHERE p.org=$1 AND p.env=registry.env.env AND p.project=$2 AND p.active)";

/// Registry statement used by [`crate::env_org::org_project_envs`].
pub const IDENTITY_PLATFORM_SRC_ORG_ORG_PROJECT_ENVS_SQL: &str = "SELECT project, env FROM registry.project_env \
    WHERE org = $1 AND active AND ($2::text IS NULL OR project = $2) ORDER BY project, env";

/// Registry statement used by [`crate::env_org::create_project`].
pub const IDENTITY_PLATFORM_SRC_ORG_CREATE_PROJECT_SQL: &str =
    "INSERT INTO registry.project (org, id) VALUES ($1, $2) ON CONFLICT (org, id) DO NOTHING";

/// Read the active deployment's suffix and its env's disposable setting.
pub const CONTROL_ENV_SRC_ENV_ANALYSE_READ_ROW: &str = "SELECT instance_suffix, disposable FROM registry.project_env p JOIN registry.env USING (org,env) \
              WHERE org = $1 AND project = $2 AND env = $3 AND p.active";

/// Read the instance suffix and drain bound before project deactivation.
pub const PROJECT_ENV_DEACTIVATE_INPUTS: &str = "SELECT instance_suffix, drain_bound_seconds FROM registry.project_env p JOIN registry.env USING (org,env) \
              WHERE org = $1 AND project = $2 AND env = $3 AND p.active";

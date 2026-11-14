//! Parameterized SQL for the workflow contract: admit, park, release, and list.
//!
//! A park moves the queue row's `available_at` to `infinity`, so the lease's
//! `available_at <= now()` never takes it. The run keeps its status
//! `dispatched`: the run-level `parked` status stays retired. Table names stay
//! unqualified, so the caller's transaction `search_path` selects the run plane,
//! and every statement reads its project from the `app.project_env_key` identity.
//!
//! Live workflow tests exercise these statements over a disposable database.

use crate::RunStatus;

/// The `available_at` of a parked queue row.
const PARKED_AT: &str = "'infinity'::timestamptz";

/// Admit a workflow run with its frozen identity and trigger metadata.
/// Binds: project, application, release, environment, workflow, version, hash,
/// principal, role, trigger, invoker, invoking user, nodes, key, input, parent, root, depth.
pub fn insert_run_sql() -> String {
    format!(
        "INSERT INTO runs (project_env_key, package_id, manifest_digest, environment, \
        wiring_id, wiring_version, wiring_hash, execution_principal_id, execution_role, \
        trigger_source, invoked_by, invoked_by_user_id, node_digests_json, idempotency_key, \
        input_json, status, event_source_run_id, event_root_run_id, event_depth) \
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8::text::uuid, $9, $10, $11, \
            $12::text::uuid, $13::text::jsonb, $14, $15::text::jsonb, '{dispatched}', $16, $17, $18) \
        ON CONFLICT (project_env_key, idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING \
        RETURNING run_id",
        dispatched = RunStatus::Dispatched.as_sql()
    )
}

/// Read an earlier admission with the same key, trigger and input.
pub fn select_run_sql() -> &'static str {
    "SELECT run_id FROM runs WHERE project_env_key = $1 AND idempotency_key = $2 \
        AND input_json = $3::text::jsonb AND trigger_source = $4"
}

/// Queue one admitted run. Binds: `$1` `project_env_key`, `$2` run id.
pub fn insert_run_queue_sql() -> &'static str {
    "INSERT INTO run_queue (project_env_key, run_id) VALUES ($1, $2)"
}

/// Park a queued run that no replica holds. It returns the run id, or no row
/// when the run is not queued, is running, or is parked already.
///
/// Binds: `$1` run id, `$2` environment.
pub fn park_queued_run_sql() -> String {
    format!(
        "UPDATE run_queue AS q SET available_at = {PARKED_AT} \
           FROM runs AS r \
          WHERE q.project_env_key = current_setting('app.project_env_key', true) \
            AND q.run_id = $1 \
            AND r.project_env_key = q.project_env_key AND r.run_id = q.run_id \
            AND r.environment = $2 \
            AND r.status = '{dispatched}' \
            AND (q.lease_expires_at IS NULL OR q.lease_expires_at <= now()) \
            AND q.available_at <> {PARKED_AT} \
          RETURNING q.run_id",
        dispatched = RunStatus::Dispatched.as_sql(),
    )
}

/// Return a parked run to the queue. It returns the run id, or no row when the
/// run is not parked.
///
/// Binds: `$1` run id, `$2` environment.
pub fn release_parked_run_sql() -> String {
    format!(
        "UPDATE run_queue AS q SET available_at = now() \
           FROM runs AS r \
          WHERE q.project_env_key = current_setting('app.project_env_key', true) \
            AND q.run_id = $1 \
            AND r.project_env_key = q.project_env_key AND r.run_id = q.run_id \
            AND r.environment = $2 \
            AND q.available_at = {PARKED_AT} \
          RETURNING q.run_id"
    )
}

/// The runs of one environment, newest first. `queued` is whether a queue row
/// exists, and `parked` whether that row is parked.
///
/// Binds: `$1` environment, `$2` limit. Columns: run id, package, wiring id,
/// wiring version, trigger source, status, queued, parked, created at (RFC
/// 3339 text).
pub fn list_workflow_runs_sql() -> String {
    format!(
        "SELECT r.run_id, r.package_id, r.wiring_id, r.wiring_version, r.trigger_source, \
                r.status, q.run_id IS NOT NULL AS queued, \
                COALESCE(q.available_at = {PARKED_AT}, false) AS parked, \
                to_char(r.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
           FROM runs AS r \
           LEFT JOIN run_queue AS q ON q.project_env_key = r.project_env_key AND q.run_id = r.run_id \
          WHERE r.project_env_key = current_setting('app.project_env_key', true) \
            AND r.environment = $1 \
            AND r.wiring_id IS NOT NULL \
          ORDER BY r.created_at DESC, r.run_id DESC \
          LIMIT $2"
    )
}

/// The facts that name why a park or a release changed nothing: the run's
/// status, whether a queue row exists, whether it is parked, and whether a
/// replica holds a live lease. No row means the run does not exist.
///
/// Binds: `$1` run id, `$2` environment.
pub fn select_run_queue_state_sql() -> String {
    format!(
        "SELECT r.status, q.run_id IS NOT NULL AS queued, \
                COALESCE(q.available_at = {PARKED_AT}, false) AS parked, \
                COALESCE(q.lease_expires_at > now(), false) AS leased \
           FROM runs AS r \
           LEFT JOIN run_queue AS q ON q.project_env_key = r.project_env_key AND q.run_id = r.run_id \
          WHERE r.project_env_key = current_setting('app.project_env_key', true) \
            AND r.run_id = $1 AND r.environment = $2"
    )
}

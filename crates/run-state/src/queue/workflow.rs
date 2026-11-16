//! Parameterized SQL for the workflow contract: admit, park, release, and list.
//!
//! A park moves the queue row's `available_at` to `infinity`, so the lease's
//! `available_at <= now()` never takes it. The run keeps its status
//! `dispatched`: the run-level `parked` status stays retired. Table names stay
//! unqualified, so the caller's transaction `search_path` selects the run plane,
//! and each caller passes the project as a bound parameter.
//!
//! Live workflow tests exercise these statements over a disposable database.

use crate::RunStatus;

/// Admit a workflow run with its frozen identity and trigger metadata.
/// Binds: project, application, release, environment, workflow, version, hash,
/// principal, role, trigger, invoker, invoking user, nodes, key, input, parent, root, depth.
pub fn insert_run_sql() -> String {
    format!(
        "INSERT INTO runs (project_env_key, package_id, manifest_digest, environment, \
        wiring_id, wiring_version, wiring_hash, execution_principal_id, execution_role, \
        trigger_source, invoked_by, invoked_by_user_id, node_digests_json, idempotency_key, \
        input_json, status, event_source_run_id, event_root_run_id, event_depth) \
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, \
            $12, $13::text::jsonb, $14, $15::text::jsonb, '{dispatched}', $16, $17, $18) \
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

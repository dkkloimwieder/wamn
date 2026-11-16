//! Parameterized SQL for the workflow contract: admit, park, release, and list.
//!
//! A park moves the queue row's `available_at` to `infinity`, so the lease's
//! `available_at <= now()` never takes it. The run keeps its status
//! `dispatched`: the run-level `parked` status stays retired. Table names stay
//! unqualified, so the caller's transaction `search_path` selects the project database.
//!
//! Live workflow tests exercise these statements over a disposable database.

use crate::RunStatus;

/// Admit a workflow run with its frozen identity and trigger metadata.
/// Binds: application, release, environment, workflow, version, hash,
/// principal, role, trigger, invoker, invoking user, nodes, key, input, parent, root, depth.
pub fn insert_run_sql() -> String {
    format!(
        "INSERT INTO runs (package_id, manifest_digest, environment, \
        wiring_id, wiring_version, wiring_hash, execution_principal_id, execution_role, \
        trigger_source, invoked_by, invoked_by_user_id, node_digests_json, idempotency_key, \
        input_json, status, event_source_run_id, event_root_run_id, event_depth) \
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
            $11, $12::text::jsonb, $13, $14::text::jsonb, '{dispatched}', $15, $16, $17) \
        ON CONFLICT (idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING \
        RETURNING run_id",
        dispatched = RunStatus::Dispatched.as_sql()
    )
}

/// Read an earlier admission with the same key, trigger and input.
pub fn select_run_sql() -> &'static str {
    "SELECT run_id FROM runs WHERE idempotency_key = $1 \
        AND input_json = $2::text::jsonb AND trigger_source = $3"
}

/// Queue one admitted run. Binds: `$1` run id.
pub fn insert_run_queue_sql() -> &'static str {
    "INSERT INTO run_queue (run_id) VALUES ($1)"
}

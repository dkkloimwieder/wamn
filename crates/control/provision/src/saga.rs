//! SQL for core provisioning-saga state.

/// Create one provisioning saga idempotently.
pub fn create_saga_sql() -> &'static str {
    "INSERT INTO provisioning.sagas (saga_id, kind, target, total_steps) \
     VALUES ($1, $2, $3, $4) \
     ON CONFLICT (saga_id) DO NOTHING"
}

/// Advance the durable provisioning checkpoint.
pub fn advance_saga_step_sql() -> &'static str {
    "UPDATE provisioning.sagas \
     SET step = step + 1, status = 'running', updated_at = now() \
     WHERE saga_id = $1"
}

/// Mark a provisioning saga complete.
pub fn complete_saga_sql() -> &'static str {
    "UPDATE provisioning.sagas \
     SET status = 'completed', updated_at = now() \
     WHERE saga_id = $1"
}

/// Mark a provisioning saga failed and retain the diagnostic.
pub fn fail_saga_sql() -> &'static str {
    "UPDATE provisioning.sagas \
     SET status = 'failed', last_error = $2, updated_at = now() \
     WHERE saga_id = $1"
}

/// Read the durable provisioning checkpoint.
pub fn select_saga_sql() -> &'static str {
    "SELECT status, step, total_steps \
     FROM provisioning.sagas \
     WHERE saga_id = $1"
}

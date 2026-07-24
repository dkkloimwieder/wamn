//! SQL for provisioning-owned saga and dump state.

pub fn create_saga_sql() -> &'static str {
    "INSERT INTO provisioning.sagas (saga_id, kind, target, total_steps) \
     VALUES ($1, $2, $3, $4) \
     ON CONFLICT (saga_id) DO NOTHING"
}

pub fn advance_saga_step_sql() -> &'static str {
    "UPDATE provisioning.sagas \
     SET step = step + 1, status = 'running', updated_at = now() \
     WHERE saga_id = $1"
}

pub fn complete_saga_sql() -> &'static str {
    "UPDATE provisioning.sagas \
     SET status = 'completed', updated_at = now() \
     WHERE saga_id = $1"
}

pub fn fail_saga_sql() -> &'static str {
    "UPDATE provisioning.sagas \
     SET status = 'failed', last_error = $2, updated_at = now() \
     WHERE saga_id = $1"
}

pub fn select_saga_sql() -> &'static str {
    "SELECT status, step, total_steps \
     FROM provisioning.sagas \
     WHERE saga_id = $1"
}

pub fn record_dump_sql() -> &'static str {
    "INSERT INTO provisioning.dumps (org, project, env, object_key, format, byte_size) \
     VALUES ($1, $2, $3, $4, $5, $6) \
     ON CONFLICT (org, project, env, object_key) DO UPDATE SET \
       format = EXCLUDED.format, \
       byte_size = EXCLUDED.byte_size, \
       taken_at = now()"
}

pub fn select_latest_dump_sql() -> &'static str {
    "SELECT object_key, format, byte_size, taken_at \
     FROM provisioning.dumps \
     WHERE org = $1 AND project = $2 AND env = $3 \
     ORDER BY taken_at DESC, object_key DESC \
     LIMIT 1"
}

pub fn select_dumps_sql() -> &'static str {
    "SELECT object_key, format, byte_size, taken_at \
     FROM provisioning.dumps \
     WHERE org = $1 AND project = $2 AND env = $3 \
     ORDER BY taken_at DESC, object_key DESC"
}

//! SQL for operations-owned copy, dump, and migration-confirmation state.

/// Stable, connection-free execution role for operations-state DML.
pub const OPS_ROLE: &str = "wamn_ops";

/// Create or harden the stable operations execution role.
///
/// The caller runs this as a role administrator before installing the artifact
/// as `wamn_system`. Tables remain owner-separated from this role.
pub fn ensure_ops_role_sql() -> &'static str {
    "DO $ops_role$ BEGIN \
       PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); \
       IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_ops') THEN \
         CREATE ROLE wamn_ops NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
           NOINHERIT NOREPLICATION NOBYPASSRLS; \
       ELSIF EXISTS (SELECT FROM pg_catalog.pg_roles \
                     WHERE rolname = 'wamn_ops' \
                       AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole \
                            OR rolinherit OR rolreplication OR rolbypassrls)) THEN \
         ALTER ROLE wamn_ops NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
           NOINHERIT NOREPLICATION NOBYPASSRLS; \
       END IF; \
     END $ops_role$;"
}

/// The schema-minted kind for a destructive-migration backup attestation.
pub const MIGRATION_CONFIRMATION_KIND: &str = "backup-checkpoint-attested";

/// Create one copy saga idempotently.
pub fn create_saga_sql() -> &'static str {
    "INSERT INTO provisioning.copy_sagas (saga_id, kind, target, total_steps) \
     VALUES ($1, $2, $3, $4) \
     ON CONFLICT (saga_id) DO NOTHING"
}

/// Advance the durable copy checkpoint.
pub fn advance_saga_step_sql() -> &'static str {
    "UPDATE provisioning.copy_sagas \
     SET step = step + 1, status = 'running', updated_at = now() \
     WHERE saga_id = $1"
}

/// Mark a copy saga complete.
pub fn complete_saga_sql() -> &'static str {
    "UPDATE provisioning.copy_sagas \
     SET status = 'completed', updated_at = now() \
     WHERE saga_id = $1"
}

/// Mark a copy saga failed and retain the diagnostic.
pub fn fail_saga_sql() -> &'static str {
    "UPDATE provisioning.copy_sagas \
     SET status = 'failed', last_error = $2, updated_at = now() \
     WHERE saga_id = $1"
}

/// Read the durable checkpoint used by the copy cutover guard.
pub fn select_saga_sql() -> &'static str {
    "SELECT status, step, total_steps \
     FROM provisioning.copy_sagas \
     WHERE saga_id = $1"
}

/// Record logical-dump metadata idempotently.
pub fn record_dump_sql() -> &'static str {
    "INSERT INTO provisioning.dumps (org, project, env, object_key, format, byte_size) \
     VALUES ($1, $2, $3, $4, $5, $6) \
     ON CONFLICT (org, project, env, object_key) DO UPDATE SET \
       format = EXCLUDED.format, \
       byte_size = EXCLUDED.byte_size, \
       taken_at = now()"
}

/// Select the newest recorded dump for one project environment.
pub fn select_latest_dump_sql() -> &'static str {
    "SELECT object_key, format, byte_size, taken_at \
     FROM provisioning.dumps \
     WHERE org = $1 AND project = $2 AND env = $3 \
     ORDER BY taken_at DESC, object_key DESC \
     LIMIT 1"
}

/// List recorded dumps for one project environment, newest first.
pub fn select_dumps_sql() -> &'static str {
    "SELECT object_key, format, byte_size, taken_at \
     FROM provisioning.dumps \
     WHERE org = $1 AND project = $2 AND env = $3 \
     ORDER BY taken_at DESC, object_key DESC"
}

/// Append a backup-checkpoint attestation for one migration identity.
pub fn record_migration_confirmation_sql() -> &'static str {
    "INSERT INTO provisioning.migration_confirmations \
       (org, project, env, tenant_id, catalog_id, from_version, to_version) \
     VALUES ($1, $2, $3, $4, $5, $6, $7) \
     ON CONFLICT (org, project, env, tenant_id, catalog_id, to_version) DO NOTHING"
}

/// Read the attestation for one migration identity.
pub fn select_migration_confirmation_sql() -> &'static str {
    "SELECT from_version, kind, confirmed_at, confirmed_by \
     FROM provisioning.migration_confirmations \
     WHERE org = $1 AND project = $2 AND env = $3 \
       AND tenant_id = $4 AND catalog_id = $5 AND to_version = $6"
}

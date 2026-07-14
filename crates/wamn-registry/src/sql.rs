//! Pure SQL text builders for the T1 control-plane registry (wamn-q3n.6).
//!
//! Registry SQL lives with the registry model (SR2: the single source, like
//! `wamn-run-store` owns the `runs` SQL), drift-guarded against the storage DDL
//! in `deploy/system-schema.sql`. Values travel as `$n` params; the driver (the
//! `provision-org` subcommand) holds the `wamn_system` connection and executes
//! the statement as the registry owner.

/// Upsert an org's placement row into `registry.orgs` (idempotent + additive —
/// re-running `provision-org` refreshes placement, never dropping). Params:
/// `$1` id, `$2` tier, `$3` prod_cluster, `$4` dev_cluster (all `text`).
///
/// The tier CHECK and the dev≠prod recovery-domain CHECK (invariant 4) are
/// enforced by the schema, not re-checked here — a bad row is rejected by the DB.
pub fn upsert_org_sql() -> &'static str {
    "INSERT INTO registry.orgs (id, tier, prod_cluster, dev_cluster) \
     VALUES ($1, $2, $3, $4) \
     ON CONFLICT (id) DO UPDATE SET \
       tier = EXCLUDED.tier, \
       prod_cluster = EXCLUDED.prod_cluster, \
       dev_cluster = EXCLUDED.dev_cluster"
}

/// Select an org's placement clusters (`prod_cluster`, `dev_cluster`) by id, so
/// `provision-project-env` (wamn-q3n.7) can pick the target cluster by the env's
/// recovery-domain [`side`](crate::Env::side) — without loading the whole
/// registry or requiring the project-env to already exist (which is what
/// [`resolve`](crate::Registry::resolve) needs). Param: `$1` org id.
pub fn select_org_clusters_sql() -> &'static str {
    "SELECT prod_cluster, dev_cluster FROM registry.orgs WHERE id = $1"
}

/// Select an org's `tier` by id, so `dump-project-env` (wamn-q3n.10) can pick the
/// dump cadence (frequency is a tier knob) without loading the whole registry.
/// Param: `$1` org id.
pub fn select_org_tier_sql() -> &'static str {
    "SELECT tier FROM registry.orgs WHERE id = $1"
}

/// Upsert a project row into `registry.projects` (idempotent). Params: `$1` org,
/// `$2` id. `ON CONFLICT (org, id) DO NOTHING` — a project carries no mutable
/// placement of its own (placement is per-env), so re-provisioning is a no-op.
pub fn upsert_project_sql() -> &'static str {
    "INSERT INTO registry.projects (org, id) VALUES ($1, $2) \
     ON CONFLICT (org, id) DO NOTHING"
}

/// List an org's provisioned project-envs (`project`, `env`, and the Secret
/// reference), so a tier move (wamn-q3n.13) can plan one dump/restore per
/// project-env without loading the whole registry. Ordered by `project, env` for a
/// stable plan. Param: `$1` org id.
pub fn select_org_project_envs_sql() -> &'static str {
    "SELECT project, env, secret_name, secret_namespace \
     FROM registry.project_envs WHERE org = $1 \
     ORDER BY project, env"
}

/// Upsert a provisioned project-env row into `registry.project_envs`. Idempotent
/// and additive — re-provisioning refreshes the credential Secret reference.
/// Params: `$1` org, `$2` project, `$3` env, `$4` secret_name, and `$5`
/// secret_namespace (nullable — `NULL` = the resolving component's own namespace).
pub fn upsert_project_env_sql() -> &'static str {
    "INSERT INTO registry.project_envs (org, project, env, secret_name, secret_namespace) \
     VALUES ($1, $2, $3, $4, $5) \
     ON CONFLICT (org, project, env) DO UPDATE SET \
       secret_name = EXCLUDED.secret_name, \
       secret_namespace = EXCLUDED.secret_namespace"
}

// --- provisioning sagas (wamn-q3n.8) ---------------------------------------
//
// The exactly-once / resumable state the provisioning orchestrator (10.1) drives
// `provision-org` / `provision-project-env` through. wamn-q3n.8 ships the pure
// builders (and the `provisionbench` gate that proves a saga row LANDS in the
// system DB per provisioned tier); the orchestrator that drives them through the
// real subcommands stays 10.1 (the `deploy/system-schema.sql` schema comment).
// The `status` literals are pinned to the `provisioning.sagas` status CHECK
// (drift-guarded against the storage DDL).

/// Create a provisioning saga (exactly-once). `INSERT … ON CONFLICT (saga_id) DO
/// NOTHING` — a redelivered create collapses onto the existing row, so a crash
/// then retry never starts a second saga. Params: `$1` saga_id, `$2` kind
/// (`provision-org` / `provision-project-env`), `$3` target (the org id or the
/// `org/project/env` triple — decoupled text, not an FK), `$4` total_steps
/// (nullable). The row starts `status='pending'`, `step=0` (schema defaults).
pub fn create_saga_sql() -> &'static str {
    "INSERT INTO provisioning.sagas (saga_id, kind, target, total_steps) \
     VALUES ($1, $2, $3, $4) \
     ON CONFLICT (saga_id) DO NOTHING"
}

/// Advance a saga's durable resume checkpoint by one step and mark it running.
/// The orchestrator runs this in the SAME txn as each step's effect (the
/// write-ahead pattern), so a crash-then-resume re-reads `step` and never
/// re-applies a committed step. Param: `$1` saga_id.
pub fn advance_saga_step_sql() -> &'static str {
    "UPDATE provisioning.sagas \
     SET step = step + 1, status = 'running', updated_at = now() \
     WHERE saga_id = $1"
}

/// Mark a saga completed (terminal success). Param: `$1` saga_id.
pub fn complete_saga_sql() -> &'static str {
    "UPDATE provisioning.sagas \
     SET status = 'completed', updated_at = now() \
     WHERE saga_id = $1"
}

/// Mark a saga failed, recording the error (terminal failure; the per-step
/// compensation ledger that unwinds it is 10.1's). Params: `$1` saga_id, `$2`
/// last_error.
pub fn fail_saga_sql() -> &'static str {
    "UPDATE provisioning.sagas \
     SET status = 'failed', last_error = $2, updated_at = now() \
     WHERE saga_id = $1"
}

// --- dump bookkeeping (wamn-q3n.10) ----------------------------------------
//
// The `provisioning.dumps` row a scheduled/on-demand per-project-env logical dump
// records when it completes (docs/postgres-topology.md §Backup architecture). The
// object key is derivable from the triple + timestamp — this row is bookkeeping,
// not the source of truth for restore (the dump CATALOG that restore reads is
// wamn-q3n.11). Columns are drift-guarded against the storage DDL.

/// Record a completed per-project-env dump (idempotent + refreshing). `ON CONFLICT
/// (org, project, env, object_key) DO UPDATE` refreshes the completed `byte_size`
/// (known only after the dump finishes) and stamps a fresh `taken_at`, so a
/// re-recorded dump updates in place rather than erroring. Params: `$1` org, `$2`
/// project, `$3` env, `$4` object_key, `$5` format, `$6` byte_size (nullable
/// `bigint`). `taken_at` is server-set (`now()`) — the clock stays in the DB.
pub fn record_dump_sql() -> &'static str {
    "INSERT INTO provisioning.dumps (org, project, env, object_key, format, byte_size) \
     VALUES ($1, $2, $3, $4, $5, $6) \
     ON CONFLICT (org, project, env, object_key) DO UPDATE SET \
       format = EXCLUDED.format, \
       byte_size = EXCLUDED.byte_size, \
       taken_at = now()"
}

// --- dump catalog read (wamn-q3n.11) ---------------------------------------
//
// The restore side (`restore-project-env`) reads the dump catalog to pick which
// dump to restore. `select_latest_dump_sql` powers **restore-to-last-dump** (no
// manual key); `select_dumps_sql` lists the window. `ORDER BY taken_at DESC,
// object_key DESC` — newest first, with the object key (which ends in the dump
// timestamp) as a deterministic tiebreak. Columns are drift-guarded against the
// storage DDL (`provisioning.dumps`).

/// Select the **latest** recorded dump for a project-env — restore-to-last-dump
/// picks it without an operator-supplied key. `ORDER BY taken_at DESC, object_key
/// DESC LIMIT 1` (newest, tiebroken by the timestamp-suffixed key). Params: `$1`
/// org, `$2` project, `$3` env. Columns: `object_key, format, byte_size, taken_at`.
pub fn select_latest_dump_sql() -> &'static str {
    "SELECT object_key, format, byte_size, taken_at \
     FROM provisioning.dumps \
     WHERE org = $1 AND project = $2 AND env = $3 \
     ORDER BY taken_at DESC, object_key DESC \
     LIMIT 1"
}

/// List all recorded dumps for a project-env, newest first (the restore window a
/// point-in-time choice ranges over). `ORDER BY taken_at DESC, object_key DESC`.
/// Params: `$1` org, `$2` project, `$3` env. Same columns as
/// [`select_latest_dump_sql`], without the `LIMIT`.
pub fn select_dumps_sql() -> &'static str {
    "SELECT object_key, format, byte_size, taken_at \
     FROM provisioning.dumps \
     WHERE org = $1 AND project = $2 AND env = $3 \
     ORDER BY taken_at DESC, object_key DESC"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_org_targets_the_orgs_columns_and_upserts() {
        let sql = upsert_org_sql();
        assert!(sql.contains("INSERT INTO registry.orgs"));
        for col in ["id", "tier", "prod_cluster", "dev_cluster"] {
            assert!(sql.contains(col), "missing column {col}");
        }
        // Values are $n params (never interpolated).
        assert!(sql.contains("VALUES ($1, $2, $3, $4)"));
        // Idempotent + additive: ON CONFLICT (id) DO UPDATE, not a plain INSERT.
        assert!(sql.contains("ON CONFLICT (id) DO UPDATE"));
    }

    #[test]
    fn select_org_clusters_reads_both_placement_clusters_by_id() {
        let sql = select_org_clusters_sql();
        assert!(sql.contains("registry.orgs"));
        assert!(sql.contains("prod_cluster") && sql.contains("dev_cluster"));
        // Keyed by the org id as a $n param (never interpolated).
        assert!(sql.contains("WHERE id = $1"));
    }

    #[test]
    fn select_org_tier_reads_the_tier_by_id() {
        let sql = select_org_tier_sql();
        assert!(sql.contains("SELECT tier FROM registry.orgs"));
        assert!(sql.contains("WHERE id = $1"));
    }

    #[test]
    fn upsert_project_targets_the_projects_columns_and_is_a_noop_on_conflict() {
        let sql = upsert_project_sql();
        assert!(sql.contains("INSERT INTO registry.projects"));
        assert!(sql.contains("(org, id)"));
        assert!(sql.contains("VALUES ($1, $2)"));
        // A project has no mutable placement — re-provisioning is a no-op.
        assert!(sql.contains("ON CONFLICT (org, id) DO NOTHING"));
    }

    #[test]
    fn select_org_project_envs_lists_an_orgs_envs_ordered() {
        let sql = select_org_project_envs_sql();
        assert!(sql.contains("FROM registry.project_envs"));
        for col in ["project", "env", "secret_name", "secret_namespace"] {
            assert!(sql.contains(col), "missing column {col}");
        }
        // Keyed by the org id as a $n param (never interpolated); stable order.
        assert!(sql.contains("WHERE org = $1"));
        assert!(sql.contains("ORDER BY project, env"));
    }

    #[test]
    fn upsert_project_env_targets_the_project_envs_columns_and_upserts() {
        let sql = upsert_project_env_sql();
        assert!(sql.contains("INSERT INTO registry.project_envs"));
        for col in ["org", "project", "env", "secret_name", "secret_namespace"] {
            assert!(sql.contains(col), "missing column {col}");
        }
        assert!(sql.contains("VALUES ($1, $2, $3, $4, $5)"));
        // Idempotent + additive: refreshes the Secret reference on the triple PK.
        assert!(sql.contains("ON CONFLICT (org, project, env) DO UPDATE"));
        assert!(sql.contains("secret_name = EXCLUDED.secret_name"));
    }

    #[test]
    fn create_saga_is_exactly_once_via_the_saga_id_pk() {
        let sql = create_saga_sql();
        assert!(sql.contains("INSERT INTO provisioning.sagas"));
        for col in ["saga_id", "kind", "target", "total_steps"] {
            assert!(sql.contains(col), "missing column {col}");
        }
        assert!(sql.contains("VALUES ($1, $2, $3, $4)"));
        // Exactly-once: a redelivered create collapses onto the existing row.
        assert!(sql.contains("ON CONFLICT (saga_id) DO NOTHING"));
    }

    #[test]
    fn advance_step_increments_the_checkpoint_and_marks_running() {
        let sql = advance_saga_step_sql();
        assert!(sql.contains("UPDATE provisioning.sagas"));
        // The durable resume checkpoint advances by exactly one.
        assert!(sql.contains("step = step + 1"));
        assert!(sql.contains("status = 'running'"));
        assert!(sql.contains("WHERE saga_id = $1"));
    }

    #[test]
    fn complete_and_fail_set_the_terminal_status() {
        let done = complete_saga_sql();
        assert!(done.contains("UPDATE provisioning.sagas"));
        assert!(done.contains("status = 'completed'"));
        assert!(done.contains("WHERE saga_id = $1"));

        let failed = fail_saga_sql();
        assert!(failed.contains("status = 'failed'"));
        // The error is a $n param (never interpolated).
        assert!(failed.contains("last_error = $2"));
        assert!(failed.contains("WHERE saga_id = $1"));
    }

    #[test]
    fn record_dump_upserts_the_dumps_columns() {
        let sql = record_dump_sql();
        assert!(sql.contains("INSERT INTO provisioning.dumps"));
        for col in ["org", "project", "env", "object_key", "format", "byte_size"] {
            assert!(sql.contains(col), "missing column {col}");
        }
        // Values are $n params (never interpolated).
        assert!(sql.contains("VALUES ($1, $2, $3, $4, $5, $6)"));
        // Idempotent + refreshing: re-recording a dump key updates byte_size in
        // place (a plain INSERT would error on the second record of the same key).
        assert!(sql.contains("ON CONFLICT (org, project, env, object_key) DO UPDATE"));
        assert!(sql.contains("byte_size = EXCLUDED.byte_size"));
    }

    #[test]
    fn select_latest_dump_reads_the_newest_dump_for_a_project_env() {
        let sql = select_latest_dump_sql();
        assert!(sql.contains("FROM provisioning.dumps"));
        // Keyed by the triple as $n params (never interpolated).
        assert!(sql.contains("WHERE org = $1 AND project = $2 AND env = $3"));
        // Newest first — taken_at DESC with the timestamp-suffixed key as tiebreak
        // (a flipped ORDER would return the OLDEST dump).
        assert!(sql.contains("ORDER BY taken_at DESC, object_key DESC"));
        assert!(sql.contains("LIMIT 1"));
        // Returns the columns restore needs (the key drives which dump to fetch).
        assert!(sql.contains("object_key"));
    }

    #[test]
    fn select_dumps_lists_the_window_newest_first() {
        let sql = select_dumps_sql();
        assert!(sql.contains("FROM provisioning.dumps"));
        assert!(sql.contains("WHERE org = $1 AND project = $2 AND env = $3"));
        // Same ordering as the latest read, but the whole window (no LIMIT).
        assert!(sql.contains("ORDER BY taken_at DESC, object_key DESC"));
        assert!(!sql.contains("LIMIT"));
    }
}

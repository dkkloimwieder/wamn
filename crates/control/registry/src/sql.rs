//! Pure SQL text builders for the T1 control-plane registry (wamn-q3n.6).
//!
//! Registry SQL lives with the registry model (SR2: the single source, like
//! `wamn-run-state` owns the `runs` SQL), drift-guarded against the storage DDL
//! in `deploy/sql/system-schema.sql`. Values travel as `$n` params; the driver (the
//! `provision-org` subcommand) holds the `wamn_system` connection and executes
//! the statement as the registry owner.

/// Upsert an org's placement row into `registry.orgs` (idempotent + additive —
/// re-running `provision-org` refreshes placement, never dropping). Params: `$1`
/// id, `$2` placement_kind (`pooled` / `dedicated`), `$3` pool_cluster (nullable
/// `text` — the shared pool for a pooled org, `NULL` for a dedicated org whose
/// clusters are derived, D18 [`cluster_of`](crate::cluster_of)).
///
/// The placement-kind CHECK and the `pooled ⟺ pool_cluster` structural CHECK are
/// enforced by the schema, not re-checked here — a bad row is rejected by the DB.
pub fn upsert_org_sql() -> &'static str {
    "INSERT INTO registry.orgs (id, placement_kind, pool_cluster) \
     VALUES ($1, $2, $3) \
     ON CONFLICT (id) DO UPDATE SET \
       placement_kind = EXCLUDED.placement_kind, \
       pool_cluster = EXCLUDED.pool_cluster"
}

/// Select an org's placement (`placement_kind`, `pool_cluster`) by id, so
/// `provision-project-env` (wamn-q3n.7) can derive the target cluster per-env via
/// [`cluster_of`](crate::cluster_of) (placement + the env policy) — without
/// loading the whole registry or requiring the project-env to already exist
/// (which is what [`resolve`](crate::Registry::resolve) needs). Param: `$1` org id.
pub fn select_org_placement_sql() -> &'static str {
    "SELECT placement_kind, pool_cluster FROM registry.orgs WHERE id = $1"
}

// --- env policies (wamn-8df.3; org-scoped by wamn-8df.4) --------------------
//
// The per-org [`EnvPolicy`](crate::EnvPolicy) rows (D18): sizing / HA / backup /
// recovery-domain per (org, env slug). `recovery_domain` is `jsonb`; the reads
// cast it to `text` so the driver serde-parses it back into `RecoveryDomain`.
// Stamped from a [`Template`](crate::Template) at provision-org time (no
// platform-global seed); columns drift-guarded against the storage DDL. The
// org's full set is what `provision-org` sizes clusters from and what
// `provision-project-env` derives the cluster owner from.

/// The `registry.env_policies` policy-value column list, in the order both reads
/// return and a row-mapper reads by index (`org` is the caller's key, not
/// returned). `recovery_domain` is cast to `text` for serde.
const ENV_POLICY_COLUMNS: &str = "name, recovery_domain::text, promotion_rank, instances, \
     storage, cpu, memory, image, backup_cadence, wal_retention, hibernation, durability_class";

/// Select one org's whole env-policy set, ordered by `promotion_rank` (so
/// `provision-org` sees `dev` before `prod`). Param: `$1` org id. Columns:
/// `ENV_POLICY_COLUMNS`.
pub fn select_env_policies_sql() -> String {
    format!(
        "SELECT {ENV_POLICY_COLUMNS} FROM registry.env_policies \
         WHERE org = $1 ORDER BY promotion_rank"
    )
}

/// Select one env policy from an org's set — `provision-project-env` reads it to
/// derive the project-env's cluster owner (and confirm the env resolves to one of
/// the org's policies). Params: `$1` org id, `$2` policy name (the env slug).
/// Columns: `ENV_POLICY_COLUMNS`.
pub fn select_env_policy_sql() -> String {
    format!(
        "SELECT {ENV_POLICY_COLUMNS} FROM registry.env_policies \
         WHERE org = $1 AND name = $2"
    )
}

/// Stamp one template policy row into an org's set — **insert-if-absent**
/// (`ON CONFLICT (org, name) DO NOTHING`), the wamn-8df.4 instantiate-and-own
/// semantics: re-running `provision-org` keeps an org's per-env customizations
/// and only adds envs the org is missing; it never clobbers a customized row
/// back to template values. Params: `$1` org, `$2` name, `$3` recovery_domain
/// (JSON text, cast `::text::jsonb`), `$4` promotion_rank, `$5` instances, `$6`
/// storage, `$7` cpu, `$8` memory, `$9` image, `$10` backup_cadence, `$11`
/// wal_retention, `$12` hibernation, `$13` durability_class.
pub fn stamp_env_policy_sql() -> &'static str {
    "INSERT INTO registry.env_policies \
       (org, name, recovery_domain, promotion_rank, instances, \
        storage, cpu, memory, image, backup_cadence, wal_retention, hibernation, \
        durability_class) \
     VALUES ($1, $2, $3::text::jsonb, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
     ON CONFLICT (org, name) DO NOTHING"
}

/// Upsert a project row into `registry.projects` (idempotent). Params: `$1` org,
/// `$2` id. `ON CONFLICT (org, id) DO NOTHING` — a project carries no mutable
/// placement of its own (placement is per-env), so re-provisioning is a no-op.
pub fn upsert_project_sql() -> &'static str {
    "INSERT INTO registry.projects (org, id) VALUES ($1, $2) \
     ON CONFLICT (org, id) DO NOTHING"
}

/// The `registry.project_envs` column list, in the order both reads return and a
/// row-mapper reads by index. `org` is the caller's key in both reads, so it is
/// not returned; the remaining columns are exactly what a
/// [`ProjectEnv`](crate::ProjectEnv) needs (the triple's `org` comes from the
/// parameter). `instance_suffix` is here because nothing else can supply it — it
/// is the one part of a derived physical name that the triple does not carry
/// (wamn-0h0g.15.89).
const PROJECT_ENV_COLUMNS: &str = "project, env, secret_name, secret_namespace, instance_suffix";

/// List an org's provisioned project-envs, so a tier move (wamn-q3n.13) can plan
/// one dump/restore per project-env — and so a consumer that derives an
/// environment's physical names can enumerate the org's instances — without
/// loading the whole registry. Ordered by `project, env` for a stable plan.
/// Param: `$1` org id. Columns: `PROJECT_ENV_COLUMNS`.
pub fn select_org_project_envs_sql() -> String {
    format!(
        "SELECT {PROJECT_ENV_COLUMNS} FROM registry.project_envs \
         WHERE org = $1 ORDER BY project, env"
    )
}

/// Read ONE provisioned project-env by its identity triple — the read a consumer
/// that derives physical names needs, since a triple alone cannot yield the
/// environment's `instance_suffix` and only the provisioner sees it come back
/// from [`upsert_project_env_sql`]. Params: `$1` org, `$2` project, `$3` env.
/// Columns: `PROJECT_ENV_COLUMNS`.
pub fn select_project_env_sql() -> String {
    format!(
        "SELECT {PROJECT_ENV_COLUMNS} FROM registry.project_envs \
         WHERE org = $1 AND project = $2 AND env = $3"
    )
}

/// List the SUPERSEDED instances of a triple, newest first — the handle a
/// reclaim pass or an operator enumeration needs (wamn-0h0g.15.90). A live
/// project-env's suffix comes from [`select_project_env_sql`]; these are the ones
/// whose row is already gone, captured by the `project_envs_retire_instance`
/// trigger. Params: `$1` org, `$2` project, `$3` env. Columns:
/// `instance_suffix, retired_at`.
pub fn select_retired_project_envs_sql() -> &'static str {
    "SELECT instance_suffix, retired_at FROM registry.retired_project_envs \
     WHERE org = $1 AND project = $2 AND env = $3 \
     ORDER BY retired_at DESC, instance_suffix"
}

/// Upsert a provisioned project-env row into `registry.project_envs`. Idempotent
/// and additive — re-provisioning refreshes the credential Secret reference.
/// Params: `$1` org, `$2` project, `$3` env, `$4` secret_name, `$5`
/// secret_namespace (nullable — `NULL` = the resolving component's own namespace),
/// `$6` the freshly minted instance_suffix, and `$7` whether this provisioning
/// declares the environment DISPOSABLE.
///
/// READ-OR-MINT: `instance_suffix` is set on INSERT and deliberately NOT
/// refreshed on conflict, and the statement RETURNS the stored value — a
/// re-provision gets back the EXISTING instance identity and keeps deriving the
/// names of the resources that already exist (wamn-0h0g.13.57). A recreated
/// environment is a new INSERT (its old row cascaded away) and gets a new one.
///
/// `disposable` is the OPPOSITE: it IS refreshed on conflict (wamn-10yt.38),
/// because unlike the instance identity it is a declared property of THIS
/// provisioning request, and a marker that could only ever be set once would
/// strand an environment on whichever side it was first provisioned. Both
/// columns come back so the caller projects the STORED row rather than the one
/// it hoped it wrote.
pub fn upsert_project_env_sql() -> &'static str {
    "INSERT INTO registry.project_envs \
       (org, project, env, secret_name, secret_namespace, instance_suffix, disposable) \
     VALUES ($1, $2, $3, $4, $5, $6, $7) \
     ON CONFLICT (org, project, env) DO UPDATE SET \
       secret_name = EXCLUDED.secret_name, \
       secret_namespace = EXCLUDED.secret_namespace, \
       disposable = EXCLUDED.disposable \
     RETURNING instance_suffix, disposable"
}

// --- event readers (wamn-l5i9.9, D19 v3) ------------------------------------
//
// The `registry.event_readers` row an `enable-cdc-project-env` overlay records:
// which publication + failover slot a project-env's CDC reader streams from,
// which JetStream stream it publishes into, and the REFERENCE to its
// replication-credential Secret (invariant 2 — never the material). Keyed by
// the (org, project, env) triple, FK'd to the provisioned project-env (the
// `provisioning.dumps` precedent), so a de-provisioned env drops its
// registration. The reader service (l5i9.10) reads its row to learn what to
// stream. Columns drift-guarded against the storage DDL.

/// Upsert a project-env's CDC reader registration (idempotent + refreshing —
/// re-enabling refreshes the names/stream/Secret reference and re-arms
/// `enabled`). Params: `$1` org, `$2` project, `$3` env, `$4` publication, `$5`
/// slot, `$6` stream, `$7` replication_secret_name, `$8`
/// replication_secret_namespace (nullable), `$9` enabled, `$10` schema, the
/// application schema that the publication covers.
pub fn upsert_event_reader_sql() -> &'static str {
    "INSERT INTO registry.event_readers \
       (org, project, env, publication, slot, stream, \
        replication_secret_name, replication_secret_namespace, enabled, schema) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
     ON CONFLICT (org, project, env) DO UPDATE SET \
       publication = EXCLUDED.publication, \
       schema = EXCLUDED.schema, \
       slot = EXCLUDED.slot, \
       stream = EXCLUDED.stream, \
       replication_secret_name = EXCLUDED.replication_secret_name, \
       replication_secret_namespace = EXCLUDED.replication_secret_namespace, \
       enabled = EXCLUDED.enabled, \
       updated_at = now()"
}

/// Read the newest capture gap of one CDC reader registration (`wamn-59z6`).
/// Params: `$1` org, `$2` project, `$3` env. Columns: `slot` (the lost slot)
/// and whether `resync_at` is set. No row: the registration never lost a slot.
pub fn select_capture_gap_sql() -> &'static str {
    "SELECT slot, resync_at IS NOT NULL \
     FROM registry.capture_gap \
     WHERE org = $1 AND project = $2 AND env = $3 \
     ORDER BY created_at DESC LIMIT 1"
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
/// org, `$2` project, `$3` env. Columns: `created_at`, `slot`, and whether
/// `resync_at` is set.
pub fn lock_newest_capture_gap_sql() -> &'static str {
    "SELECT created_at, slot, resync_at IS NOT NULL \
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

/// Read one project-env's CDC reader registration — what the reader service
/// (l5i9.10) streams by. Params: `$1` org, `$2` project, `$3` env. Columns:
/// `publication, slot, stream, replication_secret_name,
/// replication_secret_namespace, enabled, schema` (`schema` is null on a row
/// from before `migrations/system/0002_event_reader_schema.sql`).
pub fn select_event_reader_sql() -> &'static str {
    "SELECT publication, slot, stream, \
            replication_secret_name, replication_secret_namespace, enabled, schema \
     FROM registry.event_readers \
     WHERE org = $1 AND project = $2 AND env = $3"
}

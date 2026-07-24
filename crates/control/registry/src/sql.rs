//! Pure SQL text builders for the T1 control-plane registry (wamn-q3n.6).
//!
//! Registry SQL lives with the registry model (SR2: the single source, like
//! `wamn-run-store` owns the `runs` SQL), drift-guarded against the storage DDL
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
     storage, cpu, memory, image, backup_cadence, wal_retention, hibernation";

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
/// wal_retention, `$12` hibernation.
pub fn stamp_env_policy_sql() -> &'static str {
    "INSERT INTO registry.env_policies \
       (org, name, recovery_domain, promotion_rank, instances, \
        storage, cpu, memory, image, backup_cadence, wal_retention, hibernation) \
     VALUES ($1, $2, $3::text::jsonb, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
     ON CONFLICT (org, name) DO NOTHING"
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
/// replication_secret_namespace (nullable), `$9` enabled.
pub fn upsert_event_reader_sql() -> &'static str {
    "INSERT INTO registry.event_readers \
       (org, project, env, publication, slot, stream, \
        replication_secret_name, replication_secret_namespace, enabled) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
     ON CONFLICT (org, project, env) DO UPDATE SET \
       publication = EXCLUDED.publication, \
       slot = EXCLUDED.slot, \
       stream = EXCLUDED.stream, \
       replication_secret_name = EXCLUDED.replication_secret_name, \
       replication_secret_namespace = EXCLUDED.replication_secret_namespace, \
       enabled = EXCLUDED.enabled, \
       updated_at = now()"
}

/// Read one project-env's CDC reader registration — what the reader service
/// (l5i9.10) streams by. Params: `$1` org, `$2` project, `$3` env. Columns:
/// `publication, slot, stream, replication_secret_name,
/// replication_secret_namespace, enabled`.
pub fn select_event_reader_sql() -> &'static str {
    "SELECT publication, slot, stream, \
            replication_secret_name, replication_secret_namespace, enabled \
     FROM registry.event_readers \
     WHERE org = $1 AND project = $2 AND env = $3"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_org_targets_the_placement_columns_and_upserts() {
        let sql = upsert_org_sql();
        assert!(sql.contains("INSERT INTO registry.orgs"));
        for col in ["id", "placement_kind", "pool_cluster"] {
            assert!(sql.contains(col), "missing column {col}");
        }
        // Values are $n params (never interpolated) — three (D18 placement).
        assert!(sql.contains("VALUES ($1, $2, $3)"));
        // Idempotent + additive: ON CONFLICT (id) DO UPDATE, not a plain INSERT.
        assert!(sql.contains("ON CONFLICT (id) DO UPDATE"));
        // The pool cluster is refreshed on conflict (a placement change sets it).
        assert!(sql.contains("pool_cluster = EXCLUDED.pool_cluster"));
        // The retired tier / *_cluster columns are gone.
        assert!(!sql.contains("tier"));
        assert!(!sql.contains("prod_cluster"));
    }

    #[test]
    fn select_org_placement_reads_the_placement_by_id() {
        let sql = select_org_placement_sql();
        assert!(sql.contains("registry.orgs"));
        for col in ["placement_kind", "pool_cluster"] {
            assert!(sql.contains(col), "missing column {col}");
        }
        // Keyed by the org id as a $n param (never interpolated).
        assert!(sql.contains("WHERE id = $1"));
    }

    #[test]
    fn env_policy_reads_target_the_policy_columns() {
        let all = select_env_policies_sql();
        let one = select_env_policy_sql();
        for sql in [&all, &one] {
            assert!(sql.contains("FROM registry.env_policies"));
            for col in [
                "name",
                "recovery_domain",
                "promotion_rank",
                "instances",
                "storage",
                "image",
                "backup_cadence",
                "wal_retention",
                "hibernation",
            ] {
                assert!(sql.contains(col), "missing column {col}");
            }
            // recovery_domain is jsonb, read as text for serde.
            assert!(sql.contains("recovery_domain::text"));
        }
        // Policies are org-scoped (8df.4): the set read is keyed by org and
        // ordered by promotion_rank (dev before prod); the single read by
        // (org, name) — never cross-org.
        assert!(all.contains("WHERE org = $1"));
        assert!(all.contains("ORDER BY promotion_rank"));
        assert!(one.contains("WHERE org = $1 AND name = $2"));
    }

    #[test]
    fn stamp_env_policy_is_insert_if_absent_keyed_by_org_and_name() {
        let sql = stamp_env_policy_sql();
        assert!(sql.contains("INSERT INTO registry.env_policies"));
        for col in [
            "org",
            "name",
            "recovery_domain",
            "promotion_rank",
            "instances",
            "storage",
            "cpu",
            "memory",
            "image",
            "backup_cadence",
            "wal_retention",
            "hibernation",
        ] {
            assert!(sql.contains(col), "missing column {col}");
        }
        // Values are $n params; the jsonb travels as text then casts (the
        // publish-catalog `::text::jsonb` bind lesson).
        assert!(sql.contains("$3::text::jsonb"));
        assert!(sql.contains("$12"));
        // Insert-if-absent: a re-stamp NEVER clobbers an org's customization
        // (DO NOTHING, not DO UPDATE — the instantiate-and-own semantics).
        assert!(sql.contains("ON CONFLICT (org, name) DO NOTHING"));
        assert!(!sql.contains("DO UPDATE"));
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
    fn upsert_event_reader_targets_the_registration_columns_and_upserts() {
        let sql = upsert_event_reader_sql();
        assert!(sql.contains("INSERT INTO registry.event_readers"));
        for col in [
            "org",
            "project",
            "env",
            "publication",
            "slot",
            "stream",
            "replication_secret_name",
            "replication_secret_namespace",
            "enabled",
        ] {
            assert!(sql.contains(col), "missing column {col}");
        }
        // Values are $n params (never interpolated) — nine.
        assert!(sql.contains("VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"));
        // Idempotent + refreshing on the triple PK: re-enabling refreshes the
        // names/stream/Secret reference (a DO NOTHING would strand a stale row).
        assert!(sql.contains("ON CONFLICT (org, project, env) DO UPDATE"));
        assert!(sql.contains("slot = EXCLUDED.slot"));
        assert!(sql.contains("replication_secret_name = EXCLUDED.replication_secret_name"));
    }

    #[test]
    fn select_event_reader_reads_one_registration_by_triple() {
        let sql = select_event_reader_sql();
        assert!(sql.contains("FROM registry.event_readers"));
        for col in [
            "publication",
            "slot",
            "stream",
            "replication_secret_name",
            "enabled",
        ] {
            assert!(sql.contains(col), "missing column {col}");
        }
        // Keyed by the triple as $n params (never interpolated).
        assert!(sql.contains("WHERE org = $1 AND project = $2 AND env = $3"));
    }
}

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
/// and `$6` the freshly minted instance_suffix.
///
/// READ-OR-MINT: `instance_suffix` is set on INSERT and deliberately NOT
/// refreshed on conflict, and the statement RETURNS the stored value — a
/// re-provision gets back the EXISTING instance identity and keeps deriving the
/// names of the resources that already exist (wamn-0h0g.13.57). A recreated
/// environment is a new INSERT (its old row cascaded away) and gets a new one.
pub fn upsert_project_env_sql() -> &'static str {
    "INSERT INTO registry.project_envs \
       (org, project, env, secret_name, secret_namespace, instance_suffix) \
     VALUES ($1, $2, $3, $4, $5, $6) \
     ON CONFLICT (org, project, env) DO UPDATE SET \
       secret_name = EXCLUDED.secret_name, \
       secret_namespace = EXCLUDED.secret_namespace \
     RETURNING instance_suffix"
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

    const CATALOG_SCHEMA: &str = include_str!("../../../../deploy/sql/catalog-schema.sql");

    #[test]
    fn catalog_release_storage_has_stable_head_and_db_immutable_artifacts() {
        for table in [
            "flow_artifacts",
            "release_manifests",
            "release_flows",
            "catalog_heads",
        ] {
            assert!(
                CATALOG_SCHEMA.contains(&format!("CREATE TABLE catalog.{table}")),
                "missing catalog.{table}"
            );
        }
        assert!(CATALOG_SCHEMA.contains("CREATE TRIGGER flow_artifacts_immutable"));
        assert!(CATALOG_SCHEMA.contains("CREATE TRIGGER release_flows_immutable"));
        assert!(CATALOG_SCHEMA.contains("MESSAGE = 'flow-version-content-conflict'"));
        assert!(!CATALOG_SCHEMA.contains("catalog.execution_bundles"));
        assert!(CATALOG_SCHEMA.contains("PRIMARY KEY (tenant_id, catalog_id, environment)"));
        assert!(CATALOG_SCHEMA.contains("GRANT SELECT ON catalog.flow_artifacts"));
        assert!(
            !CATALOG_SCHEMA
                .contains("GRANT SELECT, INSERT, UPDATE, DELETE ON catalog.flow_artifacts")
        );
        assert!(
            CATALOG_SCHEMA
                .contains("REVOKE ALL ON FUNCTION catalog.publication_boundary(text) FROM PUBLIC")
        );
    }

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
                "durability_class",
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
            "durability_class",
        ] {
            assert!(sql.contains(col), "missing column {col}");
        }
        // Values are $n params; the jsonb travels as text then casts (the
        // publish-catalog `::text::jsonb` bind lesson).
        assert!(sql.contains("$3::text::jsonb"));
        assert!(sql.contains("$13"));
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
        for col in [
            "project",
            "env",
            "secret_name",
            "secret_namespace",
            "instance_suffix",
        ] {
            assert!(sql.contains(col), "missing column {col}");
        }
        // Keyed by the org id as a $n param (never interpolated); stable order.
        assert!(sql.contains("WHERE org = $1"));
        assert!(sql.contains("ORDER BY project, env"));
    }

    /// The single-triple read (wamn-0h0g.15.89) returns the SAME columns as the
    /// org listing — one row-mapper serves both — and is keyed by the whole triple
    /// as `$n` params. Without `instance_suffix` here, a consumer that derives an
    /// environment's physical names has no way to read the instance identity.
    #[test]
    fn select_project_env_reads_one_row_by_triple_with_the_instance_suffix() {
        let one = select_project_env_sql();
        let all = select_org_project_envs_sql();
        assert!(one.contains("FROM registry.project_envs"));
        assert!(one.contains(PROJECT_ENV_COLUMNS), "shares the column list");
        assert!(all.contains(PROJECT_ENV_COLUMNS), "shares the column list");
        assert!(one.contains("instance_suffix"));
        assert!(one.contains("WHERE org = $1 AND project = $2 AND env = $3"));
        // A single row is not an org listing — no ordering, no org-only key.
        assert!(!one.contains("ORDER BY"));
    }

    #[test]
    fn upsert_project_env_targets_the_project_envs_columns_and_upserts() {
        let sql = upsert_project_env_sql();
        assert!(sql.contains("INSERT INTO registry.project_envs"));
        for col in [
            "org",
            "project",
            "env",
            "secret_name",
            "secret_namespace",
            "instance_suffix",
        ] {
            assert!(sql.contains(col), "missing column {col}");
        }
        assert!(sql.contains("VALUES ($1, $2, $3, $4, $5, $6)"));
        // Idempotent + additive: refreshes the Secret reference on the triple PK.
        assert!(sql.contains("ON CONFLICT (org, project, env) DO UPDATE"));
        assert!(sql.contains("secret_name = EXCLUDED.secret_name"));
        // Read-or-mint: the instance identity is never refreshed, and the stored
        // value comes back so a re-provision keeps deriving the existing names.
        assert!(!sql.contains("instance_suffix = EXCLUDED.instance_suffix"));
        assert!(sql.contains("RETURNING instance_suffix"));
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

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

/// Upsert a project row into `registry.projects` (idempotent). Params: `$1` org,
/// `$2` id. `ON CONFLICT (org, id) DO NOTHING` — a project carries no mutable
/// placement of its own (placement is per-env), so re-provisioning is a no-op.
pub fn upsert_project_sql() -> &'static str {
    "INSERT INTO registry.projects (org, id) VALUES ($1, $2) \
     ON CONFLICT (org, id) DO NOTHING"
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
    fn upsert_project_targets_the_projects_columns_and_is_a_noop_on_conflict() {
        let sql = upsert_project_sql();
        assert!(sql.contains("INSERT INTO registry.projects"));
        assert!(sql.contains("(org, id)"));
        assert!(sql.contains("VALUES ($1, $2)"));
        // A project has no mutable placement — re-provisioning is a no-op.
        assert!(sql.contains("ON CONFLICT (org, id) DO NOTHING"));
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
}

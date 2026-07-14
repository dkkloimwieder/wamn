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
}

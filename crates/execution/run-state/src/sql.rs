//! The single source of run-state SQL (docs/archive/structure-review.md SR2).
//!
//! Pure text builders over the `runs` table this crate owns
//! (`deploy/sql/run-state.sql`), in the house shape: values are ALWAYS `$n`
//! parameters, identifiers are pinned, table names are UNQUALIFIED (the host
//! injects the schema via `search_path` — the S6 schema-as-fixture pattern),
//! and the tenant comes from the session claim
//! (`current_setting('app.tenant', true)`). These are run-level statements.
//! Status literals interpolate from [`crate::RunStatus`] so the builders cannot
//! drift from the model (the same discipline this crate's `queue` module uses).
//!
//! This module is guest-compilable by construction: `String` builders only,
//! no DB driver, no clock, no tokio in the dependency closure.

use crate::status::RunStatus;

/// Build the execution-only input projection for a run-row alias.
///
/// Event lineage is durable in trusted columns, never in author-visible
/// `input_json`. At single-shot execution time the runner still needs the lineage object its
/// frozen guest contract consumes, so the production-claim selector uses this
/// exact expression. The right-hand `jsonb` object replaces any same-named author
/// field; non-event rows retain their persisted input unchanged.
pub(crate) fn execution_input_sql(run_alias: &str) -> String {
    format!(
        "CASE WHEN {run_alias}.event_root_run_id IS NOT NULL \
                    AND {run_alias}.event_depth IS NOT NULL \
              THEN {run_alias}.input_json || jsonb_build_object( \
                     'causation', jsonb_build_object( \
                         'run', {run_alias}.run_id, \
                         'root', {run_alias}.event_root_run_id, \
                         'depth', {run_alias}.event_depth)) \
              ELSE {run_alias}.input_json END"
    )
}

/// Prune terminal run history older than a retention window (9.6, wamn-srb): the
/// `prune-run-history` verb's statement. DELETE `$1`'s `runs` rows in a TERMINAL
/// state ([`RunStatus::is_terminal`] — completed / failed /
/// infrastructure-failure) whose `created_at` predates `$2` days ago.
/// Any surviving `run_queue` rows cascade
/// via their `ON DELETE CASCADE` FK to `runs`. A `dispatched`/`running` run is
/// never pruned (it may still complete). Age-based
/// only in v0; no execution-lineage metadata participates.
/// Params: `$1` tenant, `$2` retention_days.
///
/// # The tenant is a BOUND PARAMETER, and that is the whole point
///
/// This predicate used to read `current_setting('app.tenant', true)`, which is
/// the RETIRED tenant keying: `wamn-0h0g.22.6` re-keyed `runs`' floor onto
/// `wamn_authority.tenant_key(tenant_id) =
/// wamn_authority.current_tenant_key()`, and the `app.tenant` claim survives
/// only on `operator_run_actions` and `run_queue`. A GUC-shaped predicate has
/// one failure mode no amount of care removes: `current_setting(_, true)`
/// returns NULL when the claim was never injected, `tenant_id = NULL` is NULL,
/// and the statement reports ZERO ROWS DELETED with no error — measured on
/// PostgreSQL 18.6 as `DELETE 0` under the dedicated retention role
/// (`wamn-0h0g.12.69`). A bound parameter cannot be forgotten: it is either
/// supplied or the statement does not execute.
///
/// Dropping the claim costs nothing the cascade needs. Measured on the same
/// server: the `run_queue` `ON DELETE CASCADE` fires as an internal
/// referential-integrity trigger that consults neither the deleter's table
/// grants nor `run_queue`'s FORCE-RLS `app.tenant` policy, so the queue row
/// still goes with the run in a session that never set the GUC and holds no
/// privilege on that relation at all.
///
/// WHAT THIS BUILDER DOES NOT DO is decide WHOSE history may be pruned. Under
/// the shared `wamn_platform` floor arm a retention credential matches every
/// tenant's rows, so binding the tenant makes the statement exact but not
/// confined. Proving that the connected identity is the one THIS tenant's
/// retention credential was minted for belongs to the verb
/// (`services/ctl/src/prune_run_history.rs`), which refuses before it ever
/// reaches this statement.
pub fn prune_terminal_runs_sql() -> String {
    let terminal: Vec<String> = RunStatus::ALL
        .into_iter()
        .filter(|s| s.is_terminal())
        .map(|s| format!("'{}'", s.as_sql()))
        .collect();
    format!(
        "DELETE FROM runs \
          WHERE tenant_id = $1 \
            AND status IN ({statuses}) \
            AND created_at < now() - ($2::bigint * interval '1 day')",
        statuses = terminal.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_input_is_transient_and_trusted_columns_replace_input_causation() {
        let expression = execution_input_sql("run_row");
        assert!(expression.starts_with("CASE WHEN run_row.event_root_run_id IS NOT NULL"));
        assert!(expression.contains("AND run_row.event_depth IS NOT NULL"));
        assert!(expression.contains("THEN run_row.input_json || jsonb_build_object("));
        assert!(expression.contains("'run', run_row.run_id"));
        assert!(expression.contains("'root', run_row.event_root_run_id"));
        assert!(expression.contains("'depth', run_row.event_depth"));
        assert!(expression.ends_with("ELSE run_row.input_json END"));
        assert!(!expression.contains("UPDATE"));
    }

    /// Every column the builders write exists in the canonical DDL — the
    /// deploy file and the builders cannot drift apart silently.
    #[test]
    fn builder_columns_exist_in_the_canonical_ddl() {
        let ddl = include_str!("../../../../deploy/sql/run-state.sql");
        for col in [
            "tenant_id",
            "run_id",
            "flow_id",
            "flow_version",
            "status",
            "trigger_source",
            "capture_mode",
            "input_json",
            "result_json",
            "state_json",
            "fail_kind",
            "updated_at",
        ] {
            assert!(ddl.contains(col), "runs column {col} missing from DDL");
        }
    }

    /// The 9.6 prune statement targets `runs`, scoped to the BOUND tenant, and
    /// only TERMINAL statuses over an age predicate — never a
    /// `running`/`dispatched` run.
    #[test]
    fn prune_targets_terminal_runs_only() {
        let sql = prune_terminal_runs_sql();
        assert!(sql.starts_with("DELETE FROM runs"), "{sql}");
        assert!(sql.contains("WHERE tenant_id = $1"), "{sql}");
        assert!(
            sql.contains("created_at < now() - ($2::bigint * interval '1 day')"),
            "{sql}"
        );
        // Exactly the terminal statuses appear; the non-terminal ones never do.
        for s in RunStatus::ALL {
            let present = sql.contains(&format!("'{}'", s.as_sql()));
            assert_eq!(
                present,
                s.is_terminal(),
                "status {} must be {} in the prune IN-list",
                s.as_sql(),
                if s.is_terminal() { "present" } else { "absent" }
            );
        }
    }
}

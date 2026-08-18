//! The single source of run-state SQL (docs/archive/structure-review.md SR2).
//!
//! Pure text builders over the `runs` / `node_runs` tables this crate owns
//! (`deploy/sql/run-state.sql`), in the house shape: values are ALWAYS `$n`
//! parameters, identifiers are pinned, table names are UNQUALIFIED (the host
//! injects the schema via `search_path` — the S6 schema-as-fixture pattern),
//! and the tenant comes from the session claim
//! (`current_setting('app.tenant', true)`). These are run-level statements;
//! mutable `node_runs` projection writes live only in the private native
//! adapter. Status literals
//! interpolate from [`crate::RunStatus`] so the builders cannot drift
//! from the model (the same discipline this crate's `queue` module uses).
//!
//! This module is guest-compilable by construction: `String` builders only,
//! no DB driver, no clock, no tokio in the dependency closure.

use wamn_pg_core::Sql;

use crate::status::RunStatus;

/// Build the execution-only input projection for a run-row alias.
///
/// Event lineage is durable in trusted columns, never in author-visible
/// `input_json`. At single-shot execution time the runner still needs the lineage object its
/// frozen guest contract consumes, so both dispatch selectors use this exact
/// expression. The right-hand `jsonb` object replaces any same-named author
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

/// [`update_run_completed_sql`] carried with its param arity (`$1..$2`).
pub fn update_run_completed() -> Sql {
    Sql::new(update_run_completed_sql(), 2)
}

/// Idempotent run open (caller-minted run id): a fresh run records its trigger
/// input; a duplicate open is a no-op. `$1` run_id, `$2` flow_id, `$3`
/// flow_version, `$4` status,
/// `$5` trigger_source (NULL for direct drivers), `$6` input_json (text the
/// server parses into jsonb).
pub fn insert_run_sql() -> String {
    "INSERT INTO runs (tenant_id, run_id, flow_id, flow_version, status, trigger_source, input_json) \
     VALUES (current_setting('app.tenant', true), $1, $2, $3, $4, $5, $6) \
     ON CONFLICT (tenant_id, run_id) DO NOTHING"
        .to_string()
}

/// The D15 write-ahead with a SERVER-minted run id: the audit row exists
/// before any node runs, and the caller learns the id from `RETURNING`.
/// `$1` flow_id, `$2` flow_version, `$3` status, `$4` trigger_source,
/// `$5` input_json.
pub fn insert_run_returning_id_sql() -> String {
    "INSERT INTO runs (tenant_id, run_id, flow_id, flow_version, status, trigger_source, input_json) \
     VALUES (current_setting('app.tenant', true), gen_random_uuid()::text, $1, $2, $3, $4, $5) \
     RETURNING run_id"
        .to_string()
}

/// Promote a dispatched run to running (the write-ahead consumed exactly
/// once — the guard keeps a replayed promotion from resurrecting a terminal
/// run). `$1` run_id.
pub fn update_run_running_sql() -> String {
    format!(
        "UPDATE runs SET status = '{running}', updated_at = now() \
         WHERE run_id = $1 AND status = '{dispatched}'",
        running = RunStatus::Running.as_sql(),
        dispatched = RunStatus::Dispatched.as_sql(),
    )
}

/// Mark the run completed and record its result payload. Deliberately
/// UNCONDITIONAL on the prior status: a genuine completion overrides a
/// janitor's premature infrastructure-failure verdict (the fqg.2 reverse-race
/// guard). `$1` run_id, `$2` result_json.
pub fn update_run_completed_sql() -> String {
    format!(
        "UPDATE runs SET status = '{completed}', result_json = $2, updated_at = now() \
         WHERE run_id = $1",
        completed = RunStatus::Completed.as_sql(),
    )
}

/// Record the run's failure verdict. `$1` run_id, `$2` fail_kind, `$3`
/// fail_node, `$4` fail_reason.
///
/// NOT GRANTED TO `wamn_app` — THIS BUILDER HAS NO CALLER (wamn-0h0g.12.40).
/// It is the only writer of `fail_node` and `fail_reason`, so when the ratified
/// `runs` UPDATE set was derived from the statements the application role
/// actually executes, those two columns fell out of it. Wiring this statement
/// up as `wamn_app` WILL fail with SQLSTATE 42501 until
/// `deploy/sql/run-state.sql` and `RUNS_APP_UPDATE_COLUMNS` in
/// `wamn_schema_control::run_plane` both name them. Adding a column to one
/// without the other makes the reconcile plan diverge, so change them together.
pub fn update_run_failed_sql() -> String {
    format!(
        "UPDATE runs SET status = '{failed}', fail_kind = $2, fail_node = $3, fail_reason = $4, \
         updated_at = now() WHERE run_id = $1",
        failed = RunStatus::Failed.as_sql(),
    )
}

/// Read an already host-claimed run's dispatch inputs — the flow it runs, the **persisted**
/// `flow_version` the run started under, and the trigger input a dispatcher
/// persisted — so the single-shot guest drives the
/// *recorded* flow at the *recorded* version, not a hard-coded fixture id and not
/// whatever version is active NOW (wamn-cox: execution pins the run's own
/// version, so a flow edited after admission cannot change its graph). `$1`
/// run_id; RLS scopes the tenant (like the other read builders).
/// Event runs receive an execution-only `causation` object synthesized from
/// trusted `event_root_run_id` / `event_depth` columns. This does not update
/// `input_json`, and the trusted object replaces any same-named input field.
/// Capture policy is read from the immutable admission row; no node-level or
/// authored fallback may replace it. A per-run `traceparent` (wamn-fl3) is the
/// natural next column added to this projection.
pub fn select_run_dispatch_sql() -> String {
    format!(
        "SELECT r.flow_id, r.flow_version, ({execution_input})::text AS input_json, \
                r.capture_mode \
           FROM runs AS r WHERE r.run_id = $1",
        execution_input = execution_input_sql("r"),
    )
}

/// Prune terminal run history older than a retention window (9.6, wamn-srb): the
/// `prune-run-history` verb's statement. DELETE the current tenant's `runs` rows
/// in a TERMINAL state ([`RunStatus::is_terminal`] — completed / failed /
/// infrastructure-failure) whose `created_at` predates `$1` days ago.
/// `node_runs` (and any surviving `run_queue` rows) cascade
/// via their `ON DELETE CASCADE` FK to `runs`. A `dispatched`/`running` run is
/// never pruned (it may still complete). Age-based
/// only in v0; no execution-lineage metadata participates.
/// Param: `$1` retention_days. RLS + the explicit tenant predicate scope it to
/// the claimed tenant, exactly like the other builders.
pub fn prune_terminal_runs_sql() -> String {
    let terminal: Vec<String> = RunStatus::ALL
        .into_iter()
        .filter(|s| s.is_terminal())
        .map(|s| format!("'{}'", s.as_sql()))
        .collect();
    format!(
        "DELETE FROM runs \
          WHERE tenant_id = current_setting('app.tenant', true) \
            AND status IN ({statuses}) \
            AND created_at < now() - ($1::bigint * interval '1 day')",
        statuses = terminal.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The highest `$n` placeholder in a builder's text — its true param count.
    fn max_placeholder(sql: &str) -> u16 {
        let bytes = sql.as_bytes();
        let mut max = 0u16;
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' {
                let mut j = i + 1;
                let mut n = 0u16;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    n = n * 10 + u16::from(bytes[j] - b'0');
                    j += 1;
                }
                if j > i + 1 {
                    max = max.max(n);
                }
                i = j;
            } else {
                i += 1;
            }
        }
        max
    }

    /// The remaining composed run-level builder pins its declared arity beside
    /// the SQL text.
    #[test]
    fn composed_builder_arities_match_their_placeholders() {
        let stmt = update_run_completed();
        assert_eq!(stmt.arity(), max_placeholder(stmt.text()));
        assert_eq!(update_run_completed().arity(), 2);
    }

    /// The builders stay in the house shape: unqualified tables, claim-scoped
    /// tenant, `$n` values only (no interpolated data), model-tied literals.
    #[test]
    fn builders_are_claim_scoped_and_parameterized() {
        for sql in [insert_run_sql(), insert_run_returning_id_sql()] {
            assert!(sql.contains("current_setting('app.tenant', true)"), "{sql}");
            assert!(
                !sql.contains("wamn_run."),
                "schema must be unqualified: {sql}"
            );
        }
        assert!(insert_run_sql().contains("ON CONFLICT (tenant_id, run_id) DO NOTHING"));
        assert!(insert_run_returning_id_sql().contains("RETURNING run_id"));
    }

    #[test]
    fn dispatch_read_projects_flow_and_input() {
        // The claim path (fqg.4) resolves the flow + input from the recorded
        // run, not a fixture constant; the persisted `flow_version` (second
        // column, wamn-cox) pins execution to the version admitted for the run;
        // fl3 extends this exact projection with `traceparent`.
        let sql = select_run_dispatch_sql();
        assert!(sql.contains("SELECT r.flow_id, r.flow_version"), "{sql}");
        assert!(sql.contains("r.capture_mode"), "{sql}");
        assert!(sql.contains("FROM runs AS r WHERE r.run_id = $1"), "{sql}");
        for trusted in [
            "r.input_json || jsonb_build_object(",
            "'run', r.run_id",
            "'root', r.event_root_run_id",
            "'depth', r.event_depth",
        ] {
            assert!(
                sql.contains(trusted),
                "missing trusted projection {trusted}: {sql}"
            );
        }
        assert!(sql.contains("ELSE r.input_json END"), "{sql}");
        assert!(
            !sql.contains("wamn_run."),
            "schema must be unqualified: {sql}"
        );
    }

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

    #[test]
    fn status_literals_come_from_the_model() {
        assert!(update_run_running_sql().contains("SET status = 'running'"));
        assert!(update_run_running_sql().contains("AND status = 'dispatched'"));
        assert!(update_run_completed_sql().contains("SET status = 'completed'"));
        assert!(
            !update_run_completed_sql().contains("AND status"),
            "completion is deliberately unconditional (fqg.2 reverse-race)"
        );
        assert!(update_run_failed_sql().contains("SET status = 'failed'"));
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
            "fail_node",
            "fail_reason",
            "updated_at",
        ] {
            assert!(ddl.contains(col), "runs column {col} missing from DDL");
        }
        for col in [
            "frame_id",
            "parent_frame_id",
            "call_site_id",
            "current_plan_hash",
            "local_node_id",
            "occurrence",
            "seq",
            "output_port",
            "output_json",
            "error_kind",
            "error_detail",
            // 9.6 capture facts persisted by the private native adapter.
            "output_size",
            "payload_hash",
        ] {
            assert!(ddl.contains(col), "node_runs column {col} missing from DDL");
        }
    }

    /// The 9.6 prune statement targets `runs` (cascading to `node_runs`), scoped
    /// to the claim, and only TERMINAL statuses over an age predicate — never a
    /// `running`/`dispatched` run.
    #[test]
    fn prune_targets_terminal_runs_only() {
        let sql = prune_terminal_runs_sql();
        assert!(sql.starts_with("DELETE FROM runs"), "{sql}");
        assert!(sql.contains("current_setting('app.tenant', true)"), "{sql}");
        assert!(
            sql.contains("created_at < now() - ($1::bigint * interval '1 day')"),
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
        assert!(
            !sql.contains("node_runs"),
            "node_runs cascades, not deleted directly"
        );
    }
}

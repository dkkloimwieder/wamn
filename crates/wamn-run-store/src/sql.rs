//! The single source of run-state SQL (docs/archive/structure-review.md SR2).
//!
//! Pure text builders over the `runs` / `node_runs` tables this crate owns
//! (`deploy/sql/run-state.sql`), in the house shape: values are ALWAYS `$n`
//! parameters, identifiers are pinned, table names are UNQUALIFIED (the host
//! injects the schema via `search_path` — the S6 schema-as-fixture pattern),
//! and the tenant comes from the session claim
//! (`current_setting('app.tenant', true)`). **Whoever holds the connection
//! executes**: the wasm guests (`flowrunner`, `poc-webhook-f1`) bind these
//! through `wamn:postgres`, host drivers through `tokio_postgres` — one SQL
//! text, never two authors of the schema's statements. Status literals
//! interpolate from [`status`](crate::status) so the builders cannot drift
//! from the model (the same discipline `wamn-run-queue` uses).
//!
//! This module is guest-compilable by construction: `String` builders only,
//! no DB driver, no clock, no tokio in the dependency closure.

use wamn_sql::Sql;

use crate::status::{NodeRunStatus, RunStatus};

// SR11: the THREE builders wamn-run-queue COMPOSES are also exposed as [`Sql`]
// (text + param arity) so the consumer renumbers its lease-renew tail against the
// arity instead of hardcoding `$7`/`$8` on an assumption about this crate. The
// arity is declared here, beside the text, and asserted against the text by
// `composed_builder_arities_match_their_placeholders` so the two cannot drift.
// The plain `*_sql` String builders stay for the direct callers (the guests, the
// benches). Other leaf builders are never composed and keep returning `String`.

/// [`update_run_completed_sql`] carried with its param arity (`$1..$2`).
pub fn update_run_completed() -> Sql {
    Sql::new(update_run_completed_sql(), 2)
}

/// [`insert_node_run_success_sql`] carried with its param arity (`$1..$7`).
pub fn insert_node_run_success() -> Sql {
    Sql::new(insert_node_run_success_sql(), 7)
}

/// [`insert_node_run_error_sql`] carried with its param arity (`$1..$8`).
pub fn insert_node_run_error() -> Sql {
    Sql::new(insert_node_run_error_sql(), 8)
}

/// Idempotent run open (caller-minted run id): a fresh run records its trigger
/// input; a resumed run is a no-op — its `node_runs` history is the durable
/// progress. `$1` run_id, `$2` flow_id, `$3` flow_version, `$4` status,
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
pub fn update_run_failed_sql() -> String {
    format!(
        "UPDATE runs SET status = '{failed}', fail_kind = $2, fail_node = $3, fail_reason = $4, \
         updated_at = now() WHERE run_id = $1",
        failed = RunStatus::Failed.as_sql(),
    )
}

/// Read the run's `state_json` (the parked-wake deadline home). `$1` run_id.
pub fn select_run_state_sql() -> String {
    "SELECT state_json::text FROM runs WHERE run_id = $1".to_string()
}

/// Read a claimed run's dispatch inputs — the flow it runs and the trigger
/// input a dispatcher persisted — so a guest that claimed the run from the queue
/// (fqg.4) drives the *recorded* flow + input, not a hard-coded fixture id. `$1`
/// run_id; RLS scopes the tenant (like the other read builders). A per-run
/// `traceparent` (wamn-fl3) is the natural next column added to this projection.
pub fn select_run_dispatch_sql() -> String {
    "SELECT flow_id, input_json::text FROM runs WHERE run_id = $1".to_string()
}

/// Persist the run's `state_json` (parking WITHOUT a `node_runs` row, so a
/// resume re-enters the parked node). `$1` run_id, `$2` state_json.
pub fn update_run_state_sql() -> String {
    "UPDATE runs SET state_json = $2, updated_at = now() WHERE run_id = $1".to_string()
}

/// Record a completed node execution — the durable per-node checkpoint,
/// written after the node's effect commits; idempotent by
/// `(run_id, node_id, occurrence)`. `occurrence` is the engine-computed visit
/// number ([`Dispatch::occurrence`](wamn_runner::Dispatch)) — a merge/loop
/// node's Nth visit is its own row, so ON CONFLICT dedupes only a REPLAY of
/// the same visit, never a distinct one (wamn-03m / cjv.10 / R24). `$1`
/// run_id, `$2` node_id, `$3` occurrence, `$4` seq, `$5` output_port,
/// `$6` output_json, `$7` input_json.
pub fn insert_node_run_success_sql() -> String {
    format!(
        "INSERT INTO node_runs \
           (tenant_id, run_id, node_id, occurrence, seq, status, output_port, output_json, input_json) \
         VALUES (current_setting('app.tenant', true), $1, $2, $3, $4, '{success}', $5, $6, $7) \
         ON CONFLICT (tenant_id, run_id, node_id, occurrence) DO NOTHING",
        success = NodeRunStatus::Success.as_sql(),
    )
}

/// Record an error-ROUTED node as an emission on the reserved `error` port
/// carrying the `{"error": {...}}` payload the engine routes — exactly what
/// 5.7 reconstruction replays (no error taxonomy needed to resume); the
/// taxonomy lands in `error_kind`/`error_detail` for the run history.
/// `$1` run_id, `$2` node_id, `$3` occurrence (the engine-computed visit),
/// `$4` seq, `$5` output_json (the error payload), `$6` input_json,
/// `$7` error_kind, `$8` error_detail.
pub fn insert_node_run_error_sql() -> String {
    format!(
        "INSERT INTO node_runs \
           (tenant_id, run_id, node_id, occurrence, seq, status, output_port, output_json, input_json, \
            error_kind, error_detail) \
         VALUES (current_setting('app.tenant', true), $1, $2, $3, $4, '{error}', 'error', $5, $6, $7, $8) \
         ON CONFLICT (tenant_id, run_id, node_id, occurrence) DO NOTHING",
        error = NodeRunStatus::Error.as_sql(),
    )
}

/// Load a run's already-completed node executions in dispatch (`seq`) order —
/// the branch-aware reconstruction source. Only `success`/`error` rows are
/// completed steps; a `parked`/`running` row is an outstanding node the walk
/// re-dispatches. `$1` run_id.
pub fn select_completed_node_runs_sql() -> String {
    format!(
        "SELECT node_id, occurrence, seq, output_port, output_json::text FROM node_runs \
         WHERE run_id = $1 AND status IN ('{success}', '{error}') ORDER BY seq",
        success = NodeRunStatus::Success.as_sql(),
        error = NodeRunStatus::Error.as_sql(),
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

    /// SR11: each composed builder's declared arity equals the highest placeholder
    /// in its own text, so a param added to the SQL without bumping the arity is
    /// caught HERE — before wamn-run-queue mis-numbers its tail against a stale
    /// arity.
    #[test]
    fn composed_builder_arities_match_their_placeholders() {
        for stmt in [
            update_run_completed(),
            insert_node_run_success(),
            insert_node_run_error(),
        ] {
            assert_eq!(
                stmt.arity(),
                max_placeholder(stmt.text()),
                "declared arity must match the text's highest $n: {}",
                stmt.text()
            );
        }
        // The exact contract wamn-run-queue composes against, pinned.
        assert_eq!(update_run_completed().arity(), 2);
        assert_eq!(insert_node_run_success().arity(), 7);
        assert_eq!(insert_node_run_error().arity(), 8);
    }

    /// The builders stay in the house shape: unqualified tables, claim-scoped
    /// tenant, `$n` values only (no interpolated data), model-tied literals.
    #[test]
    fn builders_are_claim_scoped_and_parameterized() {
        for sql in [
            insert_run_sql(),
            insert_run_returning_id_sql(),
            insert_node_run_success_sql(),
            insert_node_run_error_sql(),
        ] {
            assert!(sql.contains("current_setting('app.tenant', true)"), "{sql}");
            assert!(
                !sql.contains("wamn_run."),
                "schema must be unqualified: {sql}"
            );
        }
        assert!(insert_run_sql().contains("ON CONFLICT (tenant_id, run_id) DO NOTHING"));
        assert!(insert_run_returning_id_sql().contains("RETURNING run_id"));
        for sql in [insert_node_run_success_sql(), insert_node_run_error_sql()] {
            assert!(
                sql.contains("ON CONFLICT (tenant_id, run_id, node_id, occurrence) DO NOTHING"),
                "{sql}"
            );
            // occurrence is the $3 PARAM (the engine-computed visit), never a
            // literal 0 — a literal collapses a merge/loop node's N visits onto
            // one row and ON CONFLICT silently drops the rest (cjv.10 / R24).
            assert!(
                sql.contains("VALUES (current_setting('app.tenant', true), $1, $2, $3, $4"),
                "occurrence must bind as $3: {sql}"
            );
            assert!(!sql.contains(", 0,"), "no literal occurrence: {sql}");
        }
    }

    #[test]
    fn dispatch_read_projects_flow_and_input() {
        // The claim path (fqg.4) resolves the flow + input from the recorded
        // run, not a fixture constant; fl3 extends this exact projection with
        // `traceparent`.
        let sql = select_run_dispatch_sql();
        assert!(sql.contains("SELECT flow_id, input_json::text"), "{sql}");
        assert!(sql.contains("FROM runs WHERE run_id = $1"), "{sql}");
        assert!(
            !sql.contains("wamn_run."),
            "schema must be unqualified: {sql}"
        );
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
        assert!(insert_node_run_success_sql().contains("'success'"));
        assert!(insert_node_run_error_sql().contains("'error', 'error'"));
        assert!(select_completed_node_runs_sql().contains("IN ('success', 'error')"));
        assert!(select_completed_node_runs_sql().contains("ORDER BY seq"));
        // The reconstruction read carries the per-visit occurrence so the loaded
        // records are faithful to the rows (partial re-run selects by it).
        assert!(select_completed_node_runs_sql().contains("SELECT node_id, occurrence, seq"));
    }

    /// Every column the builders write exists in the canonical DDL — the
    /// deploy file and the builders cannot drift apart silently.
    #[test]
    fn builder_columns_exist_in_the_canonical_ddl() {
        let ddl = include_str!("../../../deploy/sql/run-state.sql");
        for col in [
            "tenant_id",
            "run_id",
            "flow_id",
            "flow_version",
            "status",
            "trigger_source",
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
            "node_id",
            "occurrence",
            "seq",
            "output_port",
            "output_json",
            "error_kind",
            "error_detail",
        ] {
            assert!(ddl.contains(col), "node_runs column {col} missing from DDL");
        }
    }
}

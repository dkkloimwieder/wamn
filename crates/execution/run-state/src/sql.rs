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
//! interpolate from [`crate::RunStatus`] so the builders cannot drift
//! from the model (the same discipline this crate's `queue` module uses).
//!
//! This module is guest-compilable by construction: `String` builders only,
//! no DB driver, no clock, no tokio in the dependency closure.

use wamn_pg_core::Sql;

use crate::status::{NodeRunStatus, RunStatus};

/// Build the execution-only input projection for a run-row alias.
///
/// Event lineage is durable in trusted columns, never in author-visible
/// `input_json`. At dispatch time the runner still needs the lineage object its
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

// SR11: the THREE builders `queue` COMPOSES are also exposed as [`Sql`]
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

/// [`insert_node_run_success_sql`] carried with its param arity (`$1..$12`).
pub fn insert_node_run_success() -> Sql {
    Sql::new(insert_node_run_success_sql(), 12)
}

/// [`insert_node_run_error_sql`] carried with its param arity (`$1..$13`).
pub fn insert_node_run_error() -> Sql {
    Sql::new(insert_node_run_error_sql(), 13)
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

/// Read the run's durable retry/context checkpoint. `$1` run_id.
pub fn select_run_state_sql() -> String {
    "SELECT state_json::text FROM runs WHERE run_id = $1".to_string()
}

/// Read a claimed run's dispatch inputs — the flow it runs, the **persisted**
/// `flow_version` the run started under, and the trigger input a dispatcher
/// persisted — so a guest that claimed the run from the queue (fqg.4) drives the
/// *recorded* flow at the *recorded* version, not a hard-coded fixture id and not
/// whatever version is active NOW (wamn-cox: a resume pins the run's own version,
/// so a flow edited mid-run cannot make a resume reconstruct against a divergent
/// graph). `$1` run_id; RLS scopes the tenant (like the other read builders). A
/// Event runs receive an execution-only `causation` object synthesized from
/// trusted `event_root_run_id` / `event_depth` columns. This does not update
/// `input_json`, and the trusted object replaces any same-named input field.
/// A per-run `traceparent` (wamn-fl3) is the natural next column added to this
/// projection.
pub fn select_run_dispatch_sql() -> String {
    format!(
        "SELECT r.flow_id, r.flow_version, ({execution_input})::text AS input_json \
           FROM runs AS r WHERE r.run_id = $1",
        execution_input = execution_input_sql("r"),
    )
}

/// Read the already selected run's pinned release members and exact plan bytes.
///
/// This is a transaction-compatible substrate read: no queue selection,
/// classification, lease grant, terminalization, or commit boundary. `$1` run id.
pub fn select_release_resolution_plans_sql() -> String {
    "SELECT member.flow_id, member.execution_bundle_hash, artifact.artifact_hash, \
            bundle.exact_bytes \
       FROM runs AS run \
       JOIN catalog.release_flows AS member \
         ON member.tenant_id = run.tenant_id \
        AND member.catalog_id = run.catalog_id \
        AND member.catalog_version = run.catalog_version \
       JOIN catalog.flow_artifacts AS artifact \
         ON artifact.tenant_id = member.tenant_id \
        AND artifact.flow_id = member.flow_id \
        AND artifact.flow_version = member.flow_version \
       JOIN catalog.execution_bundles AS bundle \
         ON bundle.tenant_id = member.tenant_id \
        AND bundle.execution_bundle_hash = member.execution_bundle_hash \
      WHERE run.tenant_id = current_setting('app.tenant', true) \
        AND run.run_id = $1 \
      ORDER BY member.flow_id"
        .to_string()
}

/// Read active connection bindings visible to a run's pinned release.
///
/// The pure resolver compares these rows with each reachable plan's recorded
/// artifact-local requirements. `$1` run id.
pub fn select_release_resolution_bindings_sql() -> String {
    "SELECT binding.artifact_hash, binding.requirement_name, \
            instance.requirement_type, instance.contract \
       FROM runs AS run \
       JOIN catalog.connection_bindings AS binding \
         ON binding.tenant_id = run.tenant_id \
        AND binding.catalog_id = run.catalog_id \
        AND binding.catalog_version = run.catalog_version \
        AND binding.environment = run.environment \
        AND binding.binding_status = 'active' \
        AND binding.validation_status = 'valid' \
       JOIN catalog.connection_instances AS instance \
         ON instance.tenant_id = binding.tenant_id \
        AND instance.environment = binding.environment \
        AND instance.instance_id = binding.instance_id \
        AND instance.lifecycle_status = 'enabled' \
       JOIN catalog.connection_generations AS generation \
         ON generation.tenant_id = instance.tenant_id \
        AND generation.environment = instance.environment \
        AND generation.instance_id = instance.instance_id \
        AND generation.generation = instance.active_generation \
      WHERE run.tenant_id = current_setting('app.tenant', true) \
        AND run.run_id = $1 \
      ORDER BY binding.artifact_hash, binding.requirement_name"
        .to_string()
}

/// Read the already selected run's validated draft candidate override.
///
/// This is a trusted loader, not caller-supplied JSON authority: every draft
/// identity value comes from the run's immutable `invocation_context` pins and
/// the run's own catalog/environment/root-flow/execution-bundle pins. `$1` run
/// id.
pub fn select_run_candidate_resolution_plan_sql() -> String {
    "SELECT draft.flow_id, draft.execution_bundle_hash, draft.draft_artifact_hash, \
            draft.binding_base_artifact_hash, bundle.exact_bytes \
       FROM runs AS run \
       JOIN catalog.validated_flow_drafts AS draft \
         ON draft.tenant_id = run.tenant_id \
        AND draft.flow_id = run.flow_id \
        AND draft.runtime_flow_version = run.flow_version \
        AND draft.catalog_id = run.catalog_id \
        AND draft.catalog_version = run.catalog_version \
        AND draft.environment = run.environment \
        AND draft.draft_artifact_hash = run.invocation_context #>> '{principal,artifact-digest}' \
        AND draft.draft_id = run.invocation_context #>> '{principal,draft-id}' \
        AND draft.draft_revision::text = run.invocation_context #>> '{principal,draft-revision}' \
        AND draft.draft_content_hash = run.invocation_context #>> '{principal,draft-content-hash}' \
        AND draft.validated_draft_hash = run.invocation_context #>> '{principal,validated-draft-hash}' \
        AND draft.execution_bundle_hash = run.execution_bundle_hash \
        AND draft.binding_base_artifact_hash = run.invocation_context #>> '{principal,binding-base-artifact-hash}' \
        AND draft.suite_flow_version::text = run.invocation_context #>> '{principal,suite-flow-version}' \
       JOIN catalog.execution_bundles AS bundle \
         ON bundle.tenant_id = draft.tenant_id \
        AND bundle.execution_bundle_hash = draft.execution_bundle_hash \
      WHERE run.tenant_id = current_setting('app.tenant', true) \
        AND run.run_id = $1 \
        AND run.trigger_source = 'scenario-draft' \
        AND run.admission_context_version = '0.1' \
        AND run.invocation_context #>> '{source,producer}' = 'draft-scenario' \
        AND run.invocation_context ->> 'version' = '0.1' \
        AND run.invocation_context #>> '{principal,tenant-id}' = run.tenant_id \
        AND run.invocation_context #>> '{principal,environment}' = run.environment \
        AND run.invocation_context #>> '{principal,catalog-id}' = run.catalog_id \
        AND run.invocation_context #>> '{principal,catalog-version}' = run.catalog_version::text \
        AND run.invocation_context #>> '{principal,run-id}' = run.run_id \
        AND run.invocation_context #>> '{principal,flow-id}' = run.flow_id \
        AND run.invocation_context #>> '{principal,flow-version}' = run.flow_version::text"
        .to_string()
}

/// Insert or verify one complete immutable run-flow resolution map. `$1` run id,
/// `$2` JSON array of `{flow-id, execution-bundle-hash, source-artifact-hash}`.
pub fn materialize_run_flow_resolutions_sql() -> String {
    "SELECT result_code, fail_kind \
       FROM materialize_run_flow_resolutions($1, $2::text::jsonb)"
        .to_string()
}

/// Persist the run's durable retry/context checkpoint. `$1` run_id, `$2`
/// state_json.
pub fn update_run_state_sql() -> String {
    "UPDATE runs SET state_json = $2, updated_at = now() WHERE run_id = $1".to_string()
}

/// Replace the durable context document while preserving co-resident checkpoint
/// cursors. `$1` run id, `$2` complete context JSON text.
pub fn update_run_context_sql() -> String {
    "UPDATE runs \
        SET state_json = jsonb_set(COALESCE(state_json, '{}'::jsonb), \
                                   '{context}', $2::text::jsonb, true), \
            updated_at = now() \
      WHERE run_id = $1"
        .to_string()
}

/// Record a completed node execution — the durable per-node checkpoint,
/// written after the node's effect commits; idempotent by
/// `(run_id, node_id, occurrence)`. `occurrence` is the engine-computed visit
/// number ([`Dispatch::occurrence`](wamn_runner::Dispatch)) — a merge/loop
/// node's Nth visit is its own row, so ON CONFLICT dedupes only a REPLAY of
/// the same visit, never a distinct one (wamn-03m / cjv.10 / R24). `$1`
/// run_id, `$2` node_id, `$3` occurrence, `$4` seq, `$5` output_port,
/// `$6` output_json, `$7` input_json, plus the 9.6 capture columns filled by
/// [`crate::capture::derive`]: `$8` preview_head, `$9` payload_size,
/// `$10` payload_hash, `$11` capture_mode, `$12` redacted. A `preview`/`off`
/// capture leaves `output_json` (`$6`) NULL, which reconstruction reads as
/// [`CaptureOff`](crate::ReconstructError::CaptureOff).
pub fn insert_node_run_success_sql() -> String {
    format!(
        "INSERT INTO node_runs \
           (tenant_id, run_id, node_id, occurrence, seq, status, output_port, output_json, input_json, \
            preview_head, payload_size, payload_hash, capture_mode, redacted) \
         VALUES (current_setting('app.tenant', true), $1, $2, $3, $4, '{success}', $5, $6, $7, \
                 $8, $9, $10, $11, $12) \
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
/// `$7` error_kind, `$8` error_detail, plus the 9.6 capture columns filled by
/// [`crate::capture::derive`] over the error payload: `$9` preview_head,
/// `$10` payload_size, `$11` payload_hash, `$12` capture_mode, `$13` redacted.
pub fn insert_node_run_error_sql() -> String {
    format!(
        "INSERT INTO node_runs \
           (tenant_id, run_id, node_id, occurrence, seq, status, output_port, output_json, input_json, \
            error_kind, error_detail, \
            preview_head, payload_size, payload_hash, capture_mode, redacted) \
         VALUES (current_setting('app.tenant', true), $1, $2, $3, $4, '{error}', 'error', $5, $6, $7, $8, \
                 $9, $10, $11, $12, $13) \
         ON CONFLICT (tenant_id, run_id, node_id, occurrence) DO NOTHING",
        error = NodeRunStatus::Error.as_sql(),
    )
}

/// Load a run's already-completed node executions in dispatch (`seq`) order —
/// the branch-aware reconstruction source. Only `success`/`error` rows are
/// completed steps; a `started` row is an outstanding node the walk
/// re-dispatches. `$1` run_id.
pub fn select_completed_node_runs_sql() -> String {
    format!(
        "SELECT node_id, occurrence, seq, output_port, output_json::text FROM node_runs \
         WHERE run_id = $1 AND status IN ('{success}', '{error}') ORDER BY seq",
        success = NodeRunStatus::Success.as_sql(),
        error = NodeRunStatus::Error.as_sql(),
    )
}

/// Prune terminal run history older than a retention window (9.6, wamn-srb): the
/// `prune-run-history` verb's statement. DELETE the current tenant's `runs` rows
/// in a TERMINAL state ([`RunStatus::is_terminal`] — completed / failed /
/// infrastructure-failure) whose `created_at` predates `$1` days ago.
/// `node_runs` (and any surviving `run_queue` / `run_dead_letters` rows) cascade
/// via their `ON DELETE CASCADE` FK to `runs`. A `dispatched`/`running` run is
/// never pruned (it may still complete). Age-based
/// only in v0 — replay lineage (`replay_of`/`root_run_id`) is not consulted.
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

    /// SR11: each composed builder's declared arity equals the highest placeholder
    /// in its own text, so a param added to the SQL without bumping the arity is
    /// caught HERE — before `queue` mis-numbers its tail against a stale
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
        // The exact contract `queue` composes against, pinned. The node-run
        // arities grew by the five 9.6 capture columns (wamn-srb): success 7 -> 12,
        // error 8 -> 13 — the composed renew tail renumbers against these
        // automatically (`queue::checkpoint_then_renew`).
        assert_eq!(update_run_completed().arity(), 2);
        assert_eq!(insert_node_run_success().arity(), 12);
        assert_eq!(insert_node_run_error().arity(), 13);
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
        // run, not a fixture constant; the persisted `flow_version` (second
        // column, wamn-cox) pins a resume to the version the run started under;
        // fl3 extends this exact projection with `traceparent`.
        let sql = select_run_dispatch_sql();
        assert!(sql.contains("SELECT r.flow_id, r.flow_version"), "{sql}");
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
    fn resolution_substrate_sql_is_transaction_compatible_and_pinned() {
        let plans = select_release_resolution_plans_sql();
        assert!(plans.contains("FROM runs AS run"), "{plans}");
        assert!(plans.contains("run.catalog_id"), "{plans}");
        assert!(plans.contains("run.catalog_version"), "{plans}");
        assert!(plans.contains("catalog.release_flows AS member"), "{plans}");
        assert!(
            plans.contains("catalog.execution_bundles AS bundle"),
            "{plans}"
        );
        assert!(plans.contains("bundle.exact_bytes"), "{plans}");
        assert!(!plans.contains("catalog_heads"), "{plans}");

        let bindings = select_release_resolution_bindings_sql();
        assert!(
            bindings.contains("catalog.connection_bindings AS binding"),
            "{bindings}"
        );
        assert!(
            bindings.contains("binding.environment = run.environment"),
            "{bindings}"
        );
        assert!(
            bindings.contains("binding.binding_status = 'active'"),
            "{bindings}"
        );
        assert!(
            bindings.contains("binding.validation_status = 'valid'"),
            "{bindings}"
        );
        assert!(
            bindings.contains("instance.lifecycle_status = 'enabled'"),
            "{bindings}"
        );

        let candidate = select_run_candidate_resolution_plan_sql();
        assert!(candidate.contains("FROM runs AS run"), "{candidate}");
        assert!(
            candidate.contains("catalog.validated_flow_drafts AS draft"),
            "{candidate}"
        );
        assert!(
            candidate.contains("catalog.execution_bundles AS bundle"),
            "{candidate}"
        );
        assert!(
            candidate.contains("draft.execution_bundle_hash = run.execution_bundle_hash"),
            "{candidate}"
        );
        for pin in [
            "draft-id",
            "draft-revision",
            "draft-content-hash",
            "validated-draft-hash",
            "binding-base-artifact-hash",
            "suite-flow-version",
            "tenant-id",
            "catalog-id",
            "catalog-version",
            "run-id",
            "flow-id",
            "flow-version",
            "artifact-digest",
        ] {
            assert!(
                candidate.contains(pin),
                "candidate loader omits {pin}: {candidate}"
            );
        }
        assert!(candidate.contains("run.admission_context_version = '0.1'"));
        assert!(candidate.contains("run.invocation_context ->> 'version' = '0.1'"));
        assert!(!candidate.contains("catalog_heads"), "{candidate}");
        assert!(!candidate.contains("$2"), "{candidate}");

        let materialize = materialize_run_flow_resolutions_sql();
        assert!(materialize.contains("materialize_run_flow_resolutions($1, $2::text::jsonb)"));
        for forbidden in [
            "BEGIN",
            "COMMIT",
            "run_queue",
            "lease_owner",
            "lease_generation",
            "lease_expires_at",
            "DELETE FROM",
            "SET status",
            "FOR UPDATE SKIP LOCKED",
        ] {
            assert!(
                !plans.contains(forbidden)
                    && !bindings.contains(forbidden)
                    && !candidate.contains(forbidden)
                    && !materialize.contains(forbidden),
                "resolution SQL must not own claim composition token {forbidden}"
            );
        }
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
        let ddl = include_str!("../../../../deploy/sql/run-state.sql");
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
            "CREATE TABLE wamn_run.run_flow_resolutions",
            "flow_id",
            "execution_bundle_hash",
            "source_artifact_hash",
            "run_flow_resolutions_update_immutable",
            "run_flow_resolutions_delete_immutable",
            "materialize_run_flow_resolutions",
        ] {
            assert!(
                ddl.contains(col),
                "run_flow_resolutions contract {col} missing from DDL"
            );
        }
        for col in [
            "node_id",
            "occurrence",
            "seq",
            "output_port",
            "output_json",
            "error_kind",
            "error_detail",
            // 9.6 capture columns the builders now write (wamn-srb).
            "preview_head",
            "payload_size",
            "payload_hash",
            "capture_mode",
            "redacted",
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
        assert!(
            !sql.contains("run_flow_resolutions"),
            "immutable resolution evidence is not pruned with terminal runs"
        );
        let ddl = include_str!("../../../../deploy/sql/run-state.sql");
        let resolutions = ddl
            .split("CREATE TABLE wamn_run.run_flow_resolutions (")
            .nth(1)
            .and_then(|tail| tail.split(");").next())
            .expect("run_flow_resolutions DDL exists");
        assert!(
            !resolutions.contains("REFERENCES wamn_run.runs"),
            "run pruning must not be blocked by a resolution-to-runs FK"
        );
    }
}

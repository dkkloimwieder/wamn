//! Parameterized SQL for the durable global FIFO queue.
//!
//! Identifiers are static, runtime values are bound, and table names remain
//! unqualified so the host-injected transaction `search_path` selects the run
//! plane. The production claim is deliberately split into small statements
//! composed by one host-owned transaction: lock, classify, then grant a lease.

use crate::durability::DURABLE_CLASS_SQL_PREDICATE;
use crate::{RunStatus, sql as run_sql};

/// Lock one eligible run in exact global FIFO order without granting a lease.
///
/// Classification occurs from the returned queue evidence before any catalog
/// read. The ordinary/pre-effect crash budget remains in force; an immutable
/// effect-attempt row escapes it only so the host can dequeue the run as
/// `effect-uncertain`. `AS MATERIALIZED` is the evaluation fence that makes
/// `LIMIT 1` exact under cached prepared-statement plans.
///
/// The effect-attempt disjunct is CLASS-GATED (wamn-0h0g.20.2): it fires only
/// for a `durable` run. On the default `standard` class the crash budget is the
/// whole eligibility story, so a budget-spent expired lease is left for the
/// janitor exactly as it would be with no effect ledger at all. The predicate is
/// correlated to `selected_run`, the row this statement already joins and locks,
/// so the gate costs no extra relation and no extra lookup.
///
/// The projection is exactly what the host composer decodes: the run id, prior
/// lease evidence, the status it validates, the authoritative input it hands
/// the router, the durability class it gates the rest of the turn on, and the
/// immutable wiring pair admission froze, plus the existing trigger-source
/// classification that tells the router whether a caller is waiting. The
/// release identity a claim records is NOT read here — it comes from the
/// claiming pod, and the lease grant writes it.
/// `$1` and `$2` are the host-carried catalog and environment scope; an
/// executor never leases another scope's FIFO row merely because the tenant
/// shares a project database.
pub fn select_production_claim_sql() -> String {
    format!(
        "WITH authority AS MATERIALIZED ( \
             SELECT require_executor_platform_authority() AS allowed \
         ), \
         candidate AS MATERIALIZED ( \
             SELECT q.tenant_id, q.run_id, \
                    q.lease_expires_at IS NOT NULL AS had_prior_lease \
               FROM run_queue AS q \
               JOIN runs AS selected_run \
                 ON selected_run.tenant_id = q.tenant_id \
                AND selected_run.run_id = q.run_id \
              WHERE (SELECT allowed FROM authority) \
                AND q.tenant_id = current_setting('app.tenant', true) \
                AND selected_run.catalog_id = $1 \
                AND selected_run.environment = $2 \
                AND q.available_at <= now() \
                AND (q.lease_expires_at IS NULL OR q.lease_expires_at <= now()) \
                AND ( \
                    q.attempts < q.max_attempts \
                    OR q.lease_expires_at IS NULL \
                    OR EXISTS ( \
                        SELECT 1 FROM effect_attempts AS effect \
                         WHERE effect.tenant_id = q.tenant_id \
                           AND effect.run_id = q.run_id \
                           AND selected_run.{durable} \
                    ) \
                ) \
              ORDER BY q.available_at, q.stream_seq, q.run_id \
              FOR UPDATE OF selected_run, q SKIP LOCKED \
              LIMIT 1 \
         ) \
         SELECT candidate.run_id, candidate.had_prior_lease, r.status, \
                ({execution_input})::text AS input_json, \
                r.durability_class, r.wiring_id, r.wiring_version, \
                COALESCE( \
                    r.trigger_source IN ('http','internal','studio') \
                    OR (r.flow_id IS NULL AND r.flow_version IS NULL \
                        AND r.trigger_source IN ('scenario-draft','test-case')), false \
                ) AS router_caller_attached, \
                COALESCE(r.trigger_source IN ('http','internal','studio'), false) \
                    AS durable_caller_attached, \
                r.flow_id, r.flow_version, r.catalog_version, \
                r.wiring_hash, r.gate_report_id, r.binding_world_json::text \
           FROM candidate \
           JOIN runs AS r \
             ON r.tenant_id = candidate.tenant_id AND r.run_id = candidate.run_id",
        execution_input = run_sql::execution_input_sql("r"),
        durable = DURABLE_CLASS_SQL_PREDICATE,
    )
}

/// Classify immutable effect-attempt evidence after the candidate locks are held.
///
/// `$1` is the selected run id. This deliberately runs as a second statement:
/// under READ COMMITTED it receives a fresh snapshot after any lock wait, so an
/// effect attempt committed before classification cannot be missed.
///
/// The caller applies the durability-class gate; the statement itself is still
/// authority-gated because only the executor claim/reap lifecycle consumes it.
pub fn select_claim_effect_attempt_sql() -> String {
    "WITH authority AS MATERIALIZED ( \
         SELECT require_executor_platform_authority() AS allowed \
     ) \
     SELECT EXISTS ( \
         SELECT 1 FROM effect_attempts AS effect \
          WHERE effect.tenant_id = current_setting('app.tenant', true) \
            AND effect.run_id = $1 \
     ) FROM authority WHERE authority.allowed"
        .to_string()
}

/// Serialize effect-attempt creation against claim-time classification.
///
/// `$1` is the run id. The tenant claim and run id form one transaction-scoped
/// advisory-lock key. Hash collisions can only add harmless contention. This
/// must be its own statement: the following effect-evidence query needs a fresh
/// READ COMMITTED snapshot after any wait on a concurrent writer.
pub fn serialize_effect_intent_sql() -> String {
    "WITH authority AS MATERIALIZED ( \
         SELECT require_executor_platform_authority() AS allowed \
     ) \
     SELECT pg_catalog.pg_advisory_xact_lock( \
         pg_catalog.hashtextextended( \
             pg_catalog.current_setting('app.tenant', true) \
                 || E'\\x1f' || $1::text, \
             0::bigint)) \
       FROM authority WHERE authority.allowed"
        .to_string()
}

/// Replace the abandoned attempt's run-level state. `$1` is the selected run id.
///
/// `state_json` and the claim-time release record are the only run columns
/// changed; immutable ledgers and the already-materialized resolution map are
/// preserved. The `(release_version, manifest_digest)` pair is projection of
/// the DEAD attempt, so it joins that replacement set (wamn-0h0g.15.55): this
/// clears it in the same transaction that re-opens the run, and the grant below
/// records the reclaiming pod's own identity fresh. Only pre-effect reclaims
/// reach this statement, so no effect was ever attributed to the pair being
/// cleared, and the database guard permits the erasure for exactly that reason.
///
pub fn clear_pre_effect_state_sql() -> String {
    "WITH authority AS MATERIALIZED ( \
         SELECT require_executor_platform_authority() AS allowed \
     ) \
     UPDATE runs \
        SET state_json = NULL, release_version = NULL, manifest_digest = NULL \
      WHERE (SELECT allowed FROM authority) \
        AND tenant_id = current_setting('app.tenant', true) AND run_id = $1 \
      RETURNING run_id"
        .to_string()
}

/// Advance the crash-evidence attempt count for the selected run.
///
/// `$1` is the selected run id. This is deliberately its OWN statement, taken
/// BEFORE the lease grant and OUTSIDE the grant's subtransaction: the grant can
/// abort (a database guard on the run row refuses the write), and an increment
/// that shared the aborted statement rolled back with it — so the run could
/// never reach `max_attempts`, the janitor could never reap it, and it stayed
/// the FIFO head forever (wamn-0h0g.15.69).
///
/// What is counted is unchanged: attempts are CRASH EVIDENCE only, so replacing
/// a prior non-NULL lease increments and a first claim or a released
/// (queue-parked) row does not. Reading `q.lease_expires_at` before the grant
/// overwrites it is what keeps that identical to the fused statement it left.
pub fn advance_claim_attempts_sql() -> String {
    "WITH authority AS MATERIALIZED ( \
         SELECT require_executor_platform_authority() AS allowed \
     ) \
     UPDATE run_queue AS q \
        SET attempts = q.attempts \
            + CASE WHEN q.lease_expires_at IS NOT NULL THEN 1 ELSE 0 END \
      WHERE (SELECT allowed FROM authority) \
        AND q.tenant_id = current_setting('app.tenant', true) \
        AND q.run_id = $1 \
      RETURNING q.attempts"
        .to_string()
}

/// Grant a lease and record the claiming pod's release identity on the run.
///
/// Params: run id, host-injected lease owner, TTL milliseconds, the pod's
/// release version, the pod's manifest digest. The crash-evidence attempt count
/// is NOT advanced here — [`advance_claim_attempts_sql`] owns it, outside this
/// statement's abort scope.
///
/// Runs are never version-pinned: the pair is minted HERE, on the write that
/// already marks the run running, never at admission and never as a second
/// statement. It is write-once PER CLAIM ATTEMPT —
/// `wamn_run.guard_run_admission_pins_immutable` permits a write over NULL and
/// refuses any differing one — so a re-claim carrying the same release is an
/// accepted no-op, and a re-claim carrying a DIFFERENT release only succeeds
/// because the arm that reopened this run's claimability already cleared the
/// abandoned attempt's pair through [`clear_pre_effect_state_sql`] in this same
/// transaction on an expired pre-effect reclaim. A pod with no injected release
/// identity binds NULL for both and records nothing.
/// The status predicate widened from `dispatched` to the two runnable states the
/// composer already validates, because a re-claim of a `running` row must reach
/// the record too; `dispatched` still becomes `running` and `running` stays put.
pub fn grant_production_claim_sql() -> String {
    format!(
        "WITH authority AS MATERIALIZED ( \
             SELECT require_executor_platform_authority() AS allowed \
         ), \
         leased AS ( \
             UPDATE run_queue AS q \
                SET lease_owner = $2, \
                    lease_expires_at = statement_timestamp() \
                        + ($3::bigint * interval '1 millisecond'), \
                    lease_generation = q.lease_generation + 1 \
              WHERE (SELECT allowed FROM authority) \
                AND q.tenant_id = current_setting('app.tenant', true) \
                AND q.run_id = $1 \
              RETURNING q.tenant_id, q.run_id, q.lease_generation \
         ), \
         marked AS ( \
             UPDATE runs AS r \
                SET status = '{running}', \
                    release_version = $4, \
                    manifest_digest = $5 \
               FROM leased \
              WHERE r.tenant_id = leased.tenant_id AND r.run_id = leased.run_id \
                AND r.status IN ('{dispatched}', '{running}') \
              RETURNING r.run_id \
         ) \
         SELECT leased.lease_generation \
           FROM leased LEFT JOIN marked ON marked.run_id = leased.run_id",
        running = RunStatus::Running.as_sql(),
        dispatched = RunStatus::Dispatched.as_sql(),
    )
}

/// Extend one live production lease under its exact owner/generation fence.
///
/// Params: run id, host-injected lease owner, lease generation, TTL
/// milliseconds. A missing row is the complete fence-lost result: the caller
/// must stop the in-flight router walk without another run-store access.
pub fn renew_production_lease_sql() -> String {
    "WITH authority AS MATERIALIZED ( \
         SELECT require_executor_platform_authority() AS allowed \
     ) \
     UPDATE run_queue AS q \
        SET lease_expires_at = statement_timestamp() \
            + ($4::bigint * interval '1 millisecond') \
      WHERE (SELECT allowed FROM authority) \
        AND q.tenant_id = current_setting('app.tenant', true) \
        AND q.run_id = $1 \
        AND q.lease_owner = $2 \
        AND q.lease_generation = $3 \
        AND q.lease_expires_at > statement_timestamp() \
      RETURNING q.lease_expires_at"
        .to_string()
}

/// Store exact effect uncertainty and dequeue without granting execution.
///
/// Params: run id, exact flat body JSON, RFC 8785 body hash. An attached,
/// unreleased caller stores HTTP 500; caller adapters map this fixed persisted
/// identity to HTTP 502. Callerless fields remain NULL and a released winner is
/// preserved exactly.
pub fn terminalize_effect_uncertain_claim_sql() -> String {
    format!(
        "WITH authority AS MATERIALIZED ( \
             SELECT require_executor_platform_authority() AS allowed \
         ), \
         updated AS ( \
             UPDATE runs AS r \
                SET status = '{uncertain}', fail_kind = '{uncertain}', \
                    caller_outcome_kind = CASE \
                        WHEN {unreleased_attached} THEN 'failed' \
                        ELSE r.caller_outcome_kind END, \
                    caller_outcome_json = CASE \
                        WHEN {unreleased_attached} THEN $2::text::jsonb \
                        ELSE r.caller_outcome_json END, \
                    caller_http_status = CASE \
                        WHEN {unreleased_attached} THEN 500 \
                        ELSE r.caller_http_status END, \
                    caller_release_node_id = CASE \
                        WHEN {unreleased_attached} THEN NULL \
                        ELSE r.caller_release_node_id END, \
                    caller_outcome_hash = CASE \
                        WHEN {unreleased_attached} THEN $3 \
                        ELSE r.caller_outcome_hash END, \
                    caller_released_at = CASE \
                        WHEN {unreleased_attached} THEN now() \
                        ELSE r.caller_released_at END, \
                    updated_at = now() \
              WHERE (SELECT allowed FROM authority) \
                AND r.tenant_id = current_setting('app.tenant', true) \
                AND r.run_id = $1 \
              RETURNING r.tenant_id, r.run_id, r.status \
         ), \
         dequeued AS ( \
             DELETE FROM run_queue AS q USING updated \
              WHERE q.tenant_id = updated.tenant_id AND q.run_id = updated.run_id \
              RETURNING q.run_id \
         ) \
         SELECT updated.status FROM updated \
          WHERE EXISTS (SELECT 1 FROM dequeued)",
        uncertain = RunStatus::EffectUncertain.as_sql(),
        unreleased_attached = "r.trigger_source IN ('http','internal','studio') \
                               AND r.caller_released_at IS NULL",
    )
}

/// Lock one crash-budget-exhausted candidate for host-owned janitor handling.
///
/// `$1` is the grace period in milliseconds; `$2` and `$3` are the trusted
/// catalog and environment scope. Effect evidence is deliberately
/// not read in this statement: the host performs the same fresh-snapshot
/// classification used by the ordinary production claimant after these locks
/// are held, so a concurrent committed effect attempt cannot be missed. The
/// durability class rides the projection for the same reason the claim's does —
/// the reaper gates that fresh-snapshot classification on it, and the run row is
/// already joined and locked here.
pub fn select_exhausted_production_sql() -> String {
    format!(
        "WITH authority AS MATERIALIZED ( \
             SELECT require_executor_platform_authority() AS allowed \
         ) \
         SELECT q.tenant_id, q.run_id, selected_run.status, \
                selected_run.flow_id, selected_run.flow_version, \
                selected_run.durability_class, selected_run.wiring_id, \
                selected_run.wiring_version \
           FROM authority CROSS JOIN run_queue AS q \
           JOIN runs AS selected_run \
             ON selected_run.tenant_id = q.tenant_id \
            AND selected_run.run_id = q.run_id \
          WHERE authority.allowed \
            AND q.tenant_id = current_setting('app.tenant', true) \
            AND selected_run.catalog_id = $2 \
            AND selected_run.environment = $3 \
                AND q.lease_expires_at IS NOT NULL \
                AND q.lease_expires_at \
                    + ($1::bigint * interval '1 millisecond') <= now() \
                AND q.attempts >= q.max_attempts \
            AND selected_run.status IN ('{dispatched}', '{running}') \
          ORDER BY q.available_at, q.stream_seq, q.run_id \
          FOR UPDATE OF selected_run, q SKIP LOCKED \
          LIMIT 1",
        dispatched = RunStatus::Dispatched.as_sql(),
        running = RunStatus::Running.as_sql(),
    )
}

/// Mark one already locked, effect-free exhausted run and dequeue it.
///
/// Params: run id, exact generic failure JSON, RFC 8785 body hash. Caller
/// outcome compare-and-set semantics match claim refusals: callerless fields
/// stay NULL and an existing released winner is preserved byte-for-byte. A
/// component-era management run stores the same truthful failure as its result
/// because it has no durable caller row for reconciliation to observe.
pub fn terminalize_exhausted_production_sql() -> String {
    format!(
        "WITH authority AS MATERIALIZED ( \
             SELECT require_executor_platform_authority() AS allowed \
         ), \
         updated AS ( \
            UPDATE runs AS r \
            SET status = '{infra}', \
                result_json = CASE \
                    WHEN {candidate_result} THEN $2::text::jsonb \
                    ELSE r.result_json END, \
                caller_outcome_kind = CASE \
                    WHEN {unreleased_attached} THEN 'failed' \
                    ELSE r.caller_outcome_kind END, \
                caller_outcome_json = CASE \
                    WHEN {unreleased_attached} THEN $2::text::jsonb \
                    ELSE r.caller_outcome_json END, \
                caller_http_status = CASE \
                    WHEN {unreleased_attached} THEN 500 \
                    ELSE r.caller_http_status END, \
                caller_release_node_id = CASE \
                    WHEN {unreleased_attached} THEN NULL \
                    ELSE r.caller_release_node_id END, \
                caller_outcome_hash = CASE \
                    WHEN {unreleased_attached} THEN $3 \
                    ELSE r.caller_outcome_hash END, \
                caller_released_at = CASE \
                    WHEN {unreleased_attached} THEN now() \
                    ELSE r.caller_released_at END, \
                updated_at = now() \
           WHERE (SELECT allowed FROM authority) \
             AND r.tenant_id = current_setting('app.tenant', true) \
             AND r.run_id = $1 \
             AND r.status IN ('{dispatched}', '{running}') \
           RETURNING r.tenant_id, r.run_id, r.status \
         ), \
         dequeued AS ( \
             DELETE FROM run_queue AS q USING updated \
              WHERE q.tenant_id = updated.tenant_id AND q.run_id = updated.run_id \
              RETURNING q.run_id \
         ) \
         SELECT updated.status FROM updated \
          WHERE EXISTS (SELECT 1 FROM dequeued)",
        infra = RunStatus::InfrastructureFailure.as_sql(),
        dispatched = RunStatus::Dispatched.as_sql(),
        running = RunStatus::Running.as_sql(),
        candidate_result = "r.flow_id IS NULL AND r.flow_version IS NULL \
                            AND r.wiring_hash IS NOT NULL \
                            AND r.gate_report_id IS NOT NULL \
                            AND r.binding_world_json IS NOT NULL",
        unreleased_attached = "r.trigger_source IN ('http','internal','studio') \
                               AND r.caller_released_at IS NULL",
    )
}

/// Reconcile due work using the production claim predicate and FIFO order.
///
/// DELIBERATELY NOT CLASS-GATED (wamn-0h0g.20.2), and it cannot be. This is the
/// dispatcher's statement, and the dispatcher runs as the scoped read role whose
/// confinement `services/dispatcher/tests/read_authority.rs` asserts: SELECT on
/// `run_queue` and `effect_attempts`, and explicitly NOT on `runs`. Correlating
/// this predicate to `runs.durability_class` — the carrier wamn-0h0g.20.1 ruled —
/// would make the dispatcher's every sweep a permission failure, so the ruling's
/// "the claim path reads ONE column" holds at the claim and not here.
///
/// Nothing is lost. What this statement produces is a WAKE HINT, not a state
/// transition: the dispatcher rings a doorbell and the executor's own claim
/// re-decides under [`select_production_claim_sql`], which IS gated. An
/// over-selected row is therefore declined at the claim, one statement later.
/// Under-selecting would be the real defect, and this predicate can only
/// over-select relative to the gated one.
pub fn parked_due_sql(limit: usize) -> String {
    format!(
        "SELECT q.run_id FROM run_queue AS q \
          WHERE q.tenant_id = current_setting('app.tenant', true) \
            AND q.available_at <= now() + interval '250 milliseconds' \
            AND (q.lease_expires_at IS NULL OR q.lease_expires_at <= now()) \
            AND ( \
                q.attempts < q.max_attempts \
                OR q.lease_expires_at IS NULL \
                OR EXISTS ( \
                    SELECT 1 FROM effect_attempts AS effect \
                     WHERE effect.tenant_id = q.tenant_id \
                       AND effect.run_id = q.run_id \
                ) \
            ) \
          ORDER BY q.available_at, q.stream_seq, q.run_id \
          LIMIT {limit}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_executor_operation_has_exactly_one_class_guard() {
        for (name, sql) in [
            ("select-production-claim", select_production_claim_sql()),
            (
                "select-claim-effect-attempt",
                select_claim_effect_attempt_sql(),
            ),
            ("serialize-effect-intent", serialize_effect_intent_sql()),
            ("clear-pre-effect-state", clear_pre_effect_state_sql()),
            ("advance-claim-attempts", advance_claim_attempts_sql()),
            ("grant-production-claim", grant_production_claim_sql()),
            ("renew-production-lease", renew_production_lease_sql()),
            (
                "terminalize-effect-uncertain",
                terminalize_effect_uncertain_claim_sql(),
            ),
            (
                "select-exhausted-production",
                select_exhausted_production_sql(),
            ),
            (
                "terminalize-exhausted-production",
                terminalize_exhausted_production_sql(),
            ),
        ] {
            assert_eq!(
                sql.matches("require_executor_platform_authority()").count(),
                1,
                "{name} must carry exactly one executor-platform guard"
            );
        }
    }

    #[test]
    fn dispatcher_wake_scan_is_not_an_executor_operation() {
        assert!(!parked_due_sql(1).contains("require_executor_platform_authority"));
    }
}

//! Parameterized SQL for the durable global FIFO queue.
//!
//! Identifiers are static, runtime values are bound, and table names remain
//! unqualified so the host-injected transaction `search_path` selects the run
//! plane. The production claim is deliberately split into small statements
//! composed by one host-owned transaction: lock, classify, then grant a lease.

use crate::{RunStatus, sql as run_sql};

/// Insert the write-ahead run row. Params: run id, flow id, flow version.
pub fn write_ahead_run_sql() -> String {
    format!(
        "INSERT INTO runs (tenant_id, run_id, flow_id, flow_version, status) \
         VALUES (current_setting('app.tenant', true), $1, $2, $3, '{dispatched}') \
         ON CONFLICT (tenant_id, run_id) DO NOTHING",
        dispatched = RunStatus::Dispatched.as_sql(),
    )
}

/// Enqueue a run. Params: run id, priority, delay milliseconds.
pub fn enqueue_sql() -> String {
    "INSERT INTO run_queue (tenant_id, run_id, priority, available_at) \
     VALUES (current_setting('app.tenant', true), $1, $2, \
             now() + ($3::bigint * interval '1 millisecond')) \
     ON CONFLICT (tenant_id, run_id) DO NOTHING"
        .to_string()
}

/// Enqueue a CDC event run. Params: run id, priority, delay, stream sequence.
pub fn enqueue_evt_sql() -> String {
    "INSERT INTO run_queue (tenant_id, run_id, priority, available_at, stream_seq) \
     VALUES (current_setting('app.tenant', true), $1, $2, \
             now() + ($3::bigint * interval '1 millisecond'), $4) \
     ON CONFLICT (tenant_id, run_id) DO NOTHING"
        .to_string()
}

/// Lock one eligible run in exact global FIFO order without granting a lease.
///
/// Classification occurs from the returned queue evidence before any catalog
/// read. The ordinary/pre-effect crash budget remains in force; an immutable
/// effect-attempt row escapes it only so the host can dequeue the run as
/// `effect-uncertain`. `AS MATERIALIZED` is the evaluation fence that makes
/// `LIMIT 1` exact under cached prepared-statement plans.
///
/// The projection is exactly what the host composer decodes: the run id, the
/// reset fence, the status it validates, and the authoritative input it hands
/// the guest. The release identity a claim records is NOT read here — it comes
/// from the claiming pod, and the lease grant writes it.
pub fn select_production_claim_sql() -> String {
    format!(
        "WITH candidate AS MATERIALIZED ( \
             SELECT q.tenant_id, q.run_id, \
                    q.lease_expires_at IS NOT NULL AS had_prior_lease, \
                    q.lease_owner, q.lease_expires_at, q.lease_generation \
               FROM run_queue AS q \
               JOIN runs AS selected_run \
                 ON selected_run.tenant_id = q.tenant_id \
                AND selected_run.run_id = q.run_id \
              WHERE q.tenant_id = current_setting('app.tenant', true) \
                AND q.available_at <= now() \
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
              FOR UPDATE OF selected_run, q SKIP LOCKED \
              LIMIT 1 \
         ) \
         SELECT candidate.run_id, candidate.had_prior_lease, \
                candidate.lease_owner, candidate.lease_expires_at::text, \
                candidate.lease_generation, r.status, \
                ({execution_input})::text AS input_json \
           FROM candidate \
           JOIN runs AS r \
             ON r.tenant_id = candidate.tenant_id AND r.run_id = candidate.run_id",
        execution_input = run_sql::execution_input_sql("r"),
    )
}

/// Classify immutable effect-attempt evidence after the candidate locks are held.
///
/// `$1` is the selected run id. This deliberately runs as a second statement:
/// under READ COMMITTED it receives a fresh snapshot after any lock wait, so an
/// effect attempt committed before classification cannot be missed.
pub fn select_claim_effect_attempt_sql() -> String {
    "SELECT EXISTS ( \
         SELECT 1 FROM effect_attempts AS effect \
          WHERE effect.tenant_id = current_setting('app.tenant', true) \
            AND effect.run_id = $1 \
     )"
    .to_string()
}

/// Whether the selected run still has mutable projection rows to reset.
pub fn select_pre_effect_projection_sql() -> String {
    "SELECT EXISTS ( \
         SELECT 1 FROM node_runs \
          WHERE tenant_id = current_setting('app.tenant', true) AND run_id = $1)"
        .to_string()
}

/// Serialize effect-attempt creation against claim-time classification.
///
/// `$1` is the run id. The tenant claim and run id form one transaction-scoped
/// advisory-lock key. Hash collisions can only add harmless contention. This
/// must be its own statement: the following effect-evidence query needs a fresh
/// READ COMMITTED snapshot after any wait on a concurrent writer.
pub fn serialize_effect_intent_sql() -> String {
    "SELECT pg_catalog.pg_advisory_xact_lock( \
         pg_catalog.hashtextextended( \
             pg_catalog.current_setting('app.tenant', true) \
                 || E'\\x1f' || $1::text, \
             0::bigint))"
        .to_string()
}

/// Replace the abandoned attempt's run-level projection only after projection
/// deletion. `$1` is the selected run id.
///
/// `state_json` and the claim-time release record are the only run columns
/// changed; immutable ledgers and the already-materialized resolution map are
/// preserved. The `(release_version, manifest_digest)` pair is projection of
/// the DEAD attempt, so it joins that replacement set (wamn-0h0g.15.55): this
/// clears it in the same transaction that re-opens the run, and the grant below
/// records the reclaiming pod's own identity fresh. Only pre-effect reclaims
/// reach this statement, so no execution used the pair being cleared, and the
/// database guard permits the erasure for exactly that reason.
pub fn clear_pre_effect_state_sql() -> String {
    "UPDATE runs \
        SET state_json = NULL, release_version = NULL, manifest_digest = NULL \
      WHERE tenant_id = current_setting('app.tenant', true) AND run_id = $1 \
        AND NOT EXISTS ( \
            SELECT 1 FROM node_runs \
             WHERE tenant_id = current_setting('app.tenant', true) AND run_id = $1) \
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
    "UPDATE run_queue AS q \
        SET attempts = q.attempts \
            + CASE WHEN q.lease_expires_at IS NOT NULL THEN 1 ELSE 0 END \
      WHERE q.tenant_id = current_setting('app.tenant', true) \
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
/// because [`clear_pre_effect_state_sql`] already cleared the abandoned
/// attempt's pair in this transaction. A pod with no injected release identity
/// binds NULL for both and records nothing.
/// The status predicate widened from `dispatched` to the two runnable states the
/// composer already validates, because a re-claim of a `running` row must reach
/// the record too; `dispatched` still becomes `running` and `running` stays put.
pub fn grant_production_claim_sql() -> String {
    format!(
        "WITH leased AS ( \
             UPDATE run_queue AS q \
                SET lease_owner = $2, \
                    lease_expires_at = statement_timestamp() \
                        + ($3::bigint * interval '1 millisecond'), \
                    lease_generation = q.lease_generation + 1 \
              WHERE q.tenant_id = current_setting('app.tenant', true) \
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

/// Store exact effect uncertainty and dequeue without granting execution.
///
/// Params: run id, exact flat body JSON, RFC 8785 body hash. An attached,
/// unreleased caller stores HTTP 500; caller adapters map this fixed persisted
/// identity to HTTP 502. Callerless fields remain NULL and a released winner is
/// preserved exactly.
pub fn terminalize_effect_uncertain_claim_sql() -> String {
    format!(
        "WITH updated AS ( \
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
              WHERE r.tenant_id = current_setting('app.tenant', true) \
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

/// Atomically mark a completed run and remove its queue row.
pub fn complete_dequeue_sql() -> String {
    format!(
        "WITH done AS ({completed}) {dequeue}",
        completed = run_sql::update_run_completed().text(),
        dequeue = dequeue_sql(),
    )
}

/// Change `dispatched` to `running`; reclaiming `running` is a no-op.
pub fn mark_running_sql() -> String {
    format!(
        "UPDATE runs SET status = '{running}' \
          WHERE tenant_id = current_setting('app.tenant', true) \
            AND run_id = $1 AND status = '{dispatched}'",
        running = RunStatus::Running.as_sql(),
        dispatched = RunStatus::Dispatched.as_sql(),
    )
}

/// Renew a held lease. Params: run id, TTL milliseconds, owner.
pub fn renew_lease_sql() -> String {
    "UPDATE run_queue \
        SET lease_expires_at = now() + ($2::bigint * interval '1 millisecond') \
      WHERE tenant_id = current_setting('app.tenant', true) \
        AND run_id = $1 AND lease_owner = $3"
        .to_string()
}

/// Remove a run's queue row. `$1` is the run id.
pub fn dequeue_sql() -> String {
    "DELETE FROM run_queue \
      WHERE tenant_id = current_setting('app.tenant', true) AND run_id = $1"
        .to_string()
}

/// Wait until a later queue time and release the current lease.
pub fn park_sql() -> String {
    "UPDATE run_queue \
        SET available_at = now() + ($2::bigint * interval '1 millisecond'), \
            lease_owner = NULL, lease_expires_at = NULL \
      WHERE tenant_id = current_setting('app.tenant', true) AND run_id = $1"
        .to_string()
}

/// Lock one crash-budget-exhausted candidate for host-owned janitor handling.
///
/// `$1` is the grace period in milliseconds. Effect evidence is deliberately
/// not read in this statement: the host performs the same fresh-snapshot
/// classification used by the ordinary production claimant after these locks
/// are held, so a concurrent committed effect attempt cannot be missed.
pub fn select_exhausted_production_sql() -> String {
    format!(
        "SELECT q.tenant_id, q.run_id, selected_run.status, \
                selected_run.flow_id, selected_run.flow_version \
           FROM run_queue AS q \
           JOIN runs AS selected_run \
             ON selected_run.tenant_id = q.tenant_id \
            AND selected_run.run_id = q.run_id \
          WHERE q.tenant_id = current_setting('app.tenant', true) \
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
/// Params: run id, exact generic caller body JSON, RFC 8785 body hash. Caller
/// outcome compare-and-set semantics match claim refusals: callerless fields
/// stay NULL and an existing released winner is preserved byte-for-byte.
pub fn terminalize_exhausted_production_sql() -> String {
    format!(
        "WITH updated AS ( \
             UPDATE runs AS r \
            SET status = '{infra}', \
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
           WHERE r.tenant_id = current_setting('app.tenant', true) \
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
        unreleased_attached = "r.trigger_source IN ('http','internal','studio') \
                               AND r.caller_released_at IS NULL",
    )
}

/// Insert a triggered write-ahead run row.
pub fn write_ahead_triggered_run_sql() -> String {
    format!(
        "INSERT INTO runs \
             (tenant_id, run_id, flow_id, flow_version, status, trigger_source, input_json) \
         VALUES (current_setting('app.tenant', true), $1, $2, $3, \
                 '{dispatched}', $4, $5::text::jsonb) \
         ON CONFLICT (tenant_id, run_id) DO NOTHING",
        dispatched = RunStatus::Dispatched.as_sql(),
    )
}

/// Read active flow declarations for trigger dispatch.
pub fn active_flows_sql() -> String {
    "SELECT flow_id, version, graph_json::text AS graph_json FROM flows \
      WHERE tenant_id = current_setting('app.tenant', true) AND active"
        .to_string()
}

/// Reconcile due work using the production claim predicate and FIFO order.
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
    fn composed_arity_flows_from_the_producing_crate() {
        assert_eq!(run_sql::update_run_completed().arity(), 2);
        let completed = complete_dequeue_sql();
        assert!(completed.contains(run_sql::update_run_completed().text()));
        assert!(completed.contains(&dequeue_sql()));
    }
}

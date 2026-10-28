//! Parameterized SQL for the durable global FIFO queue.
//!
//! Identifiers are static, runtime values are bound, and table names remain
//! unqualified so the host-injected transaction `search_path` selects the run
//! plane. The production claim is deliberately split into small statements
//! composed by one host-owned transaction: lock, clear abandoned state, then lease.

use super::MAX_RETRY_BACKOFF_MS;
use crate::{FailType, RunStatus, sql as run_sql};

/// Lock one eligible run in exact global FIFO order without granting a lease.
///
/// The shared retry budget excludes exhausted expired leases for the janitor.
/// Credential releases at the limit end the run before another claim. Operator
/// wakes remain eligible even when the budget is spent.
/// `AS MATERIALIZED` keeps `LIMIT 1` exact under cached prepared-statement plans.
///
/// The projection is exactly what the host composer decodes: the run id, prior
/// lease evidence, the status it validates, the authoritative input it hands
/// the router, and the immutable wiring pair admission froze, plus the trigger-source
/// classification that tells the router whether a caller is waiting, and the
/// selected row's package id. A manual workflow run adds what its queue request
/// needs: its trigger source, its executing principal and active role, and the
/// component digest of every node
/// (docs/architecture/execution.md#environment-workflows). The release is
/// admission-pinned by its manifest digest, which the grant compares with the
/// claiming pod's digest. `$1` is the exact package-id set in the mounted
/// release and `$2` is the environment scope. One set-valued predicate
/// preserves the tenant's single global FIFO; an executor never leases a
/// package outside its release merely because the tenant shares a project
/// database.
///
/// `$3` is the claiming host's release manifest digest, or NULL for a host that
/// carries none. A run executes only on hosts of the release it was admitted
/// under, so the select takes only runs whose pin equals `$3`: a production
/// host, which always carries a digest, selects exactly its own release's
/// runs, and a run pinned to another release never becomes its head. While two
/// releases coexist, FIFO is per release and a draining host claims only its
/// own backlog (platform-deploy.md R20). A candidate run carries no pin, so
/// only a host with no release selects it: candidate selection keeps its own
/// path. [`grant_production_claim_sql`] rechecks the pin.
pub fn select_production_claim_sql() -> String {
    format!(
        "WITH candidate AS MATERIALIZED ( \
             SELECT q.env_key, q.run_id, \
                    q.lease_expires_at IS NOT NULL AS had_prior_lease \
               FROM run_queue AS q \
               JOIN runs AS selected_run \
                 ON selected_run.env_key = q.env_key \
                AND selected_run.run_id = q.run_id \
              WHERE q.env_key = current_setting('app.env_key', true) \
                AND selected_run.package_id = ANY($1::text[]) \
                AND selected_run.environment = $2 \
                AND selected_run.manifest_digest IS NOT DISTINCT FROM $3::text \
                AND q.available_at <= now() \
                AND (q.lease_expires_at IS NULL OR q.lease_expires_at <= now()) \
                AND ( \
                    q.attempts < q.max_attempts \
                    OR q.lease_expires_at IS NULL \
                ) \
              ORDER BY q.available_at, q.stream_seq, q.run_id \
              FOR UPDATE OF selected_run, q SKIP LOCKED \
              LIMIT 1 \
         ) \
         SELECT candidate.run_id, candidate.had_prior_lease, r.status, \
                ({execution_input})::text AS input_json, \
                r.wiring_id, r.wiring_version, \
                COALESCE( \
                    r.trigger_source IN ('http','internal','studio') \
                    OR (r.flow_id IS NULL AND r.flow_version IS NULL \
                        AND r.trigger_source IN ('scenario-draft','test-case')), false \
                ) AS router_caller_attached, \
                COALESCE(r.trigger_source IN ('http','internal','studio'), false) \
                    AS durable_caller_attached, \
                r.flow_id, r.flow_version, \
                r.wiring_hash, r.binding_world_json::text, r.package_id, \
                r.service_principal_id::text, r.trigger_source, \
                r.execution_principal_id::text, r.execution_role, \
                r.node_digests_json::text, r.event_root_run_id, r.event_depth \
           FROM candidate \
           JOIN runs AS r \
             ON r.env_key = candidate.env_key AND r.run_id = candidate.run_id",
        execution_input = run_sql::execution_input_sql("r"),
    )
}

/// Replace the abandoned attempt's run-level state. `$1` is the selected run id.
///
/// `state_json` is the only run column changed; immutable tables and the
/// already-materialized resolution map are preserved. The release digest is
/// admission identity and is never cleared.
///
pub fn clear_pre_effect_state_sql() -> String {
    "UPDATE runs \
        SET state_json = NULL \
      WHERE env_key = current_setting('app.env_key', true) AND run_id = $1 \
      RETURNING run_id"
        .to_string()
}

/// Count an expired lease against the shared retry budget.
///
/// `$1` is the selected run id. This is deliberately its OWN statement, taken
/// BEFORE the lease grant and OUTSIDE the grant's subtransaction: the grant can
/// abort (a database guard on the run row refuses the write), and an increment
/// that shared the aborted statement rolled back with it — so the run could
/// never reach `max_attempts`, the janitor could never reap it, and it stayed
/// the FIFO head forever (wamn-0h0g.15.69).
///
/// A prior non-NULL lease increments the shared count. First claims and
/// released rows do not. Credential releases spend this same count when they
/// release the lease, so the next claim does not count that failure twice.
pub fn advance_claim_attempts_sql() -> String {
    "UPDATE run_queue AS q \
        SET attempts = q.attempts \
            + CASE WHEN q.lease_expires_at IS NOT NULL THEN 1 ELSE 0 END \
      WHERE q.env_key = current_setting('app.env_key', true) \
        AND q.run_id = $1 \
      RETURNING q.attempts"
        .to_string()
}

/// Grant a lease and verify the claiming release.
///
/// Params: run id, host-injected lease owner, TTL milliseconds, the pod's
/// manifest digest. The crash-evidence attempt count is NOT advanced here —
/// [`advance_claim_attempts_sql`] owns it, outside this statement's abort scope.
///
/// The release digest was pinned at admission. The claim verifies the pod
/// carries that same release on the write that marks the run running. A pod
/// with no injected release identity binds NULL and verifies nothing.
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
              WHERE q.env_key = current_setting('app.env_key', true) \
                AND q.run_id = $1 \
              RETURNING q.env_key, q.run_id, q.lease_generation \
         ), \
         marked AS ( \
             UPDATE runs AS r \
                SET status = '{running}' \
               FROM leased \
              WHERE r.env_key = leased.env_key AND r.run_id = leased.run_id \
                AND r.status IN ('{dispatched}', '{running}') \
                AND ($4::text IS NULL OR r.manifest_digest = $4) \
              RETURNING r.run_id \
         ) \
         SELECT leased.lease_generation \
           FROM leased JOIN marked ON marked.run_id = leased.run_id",
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
    "UPDATE run_queue AS q \
        SET lease_expires_at = statement_timestamp() \
            + ($4::bigint * interval '1 millisecond') \
      WHERE q.env_key = current_setting('app.env_key', true) \
        AND q.run_id = $1 \
        AND q.lease_owner = $2 \
        AND q.lease_generation = $3 \
        AND q.lease_expires_at > statement_timestamp() \
      RETURNING q.lease_expires_at"
        .to_string()
}

/// Release one live claim after a credential or pool failure.
/// Binds: run id, lease owner, lease generation. The statement locks both rows
/// until release or terminalization commits. It returns exhaustion and wiring.
/// Live tests cover fencing, the shared count, and the delay bounds.
pub fn release_production_claim_sql() -> String {
    format!(
        "WITH held AS MATERIALIZED ( \
             SELECT q.env_key, q.run_id, r.wiring_id, r.wiring_version \
               FROM run_queue AS q JOIN runs AS r \
                 ON r.env_key = q.env_key AND r.run_id = q.run_id \
              WHERE q.env_key = current_setting('app.env_key', true) \
                AND q.run_id = $1 AND q.lease_owner = $2 \
                AND q.lease_generation = $3 \
                AND q.lease_expires_at > statement_timestamp() \
                AND r.status IN ('dispatched', 'running') \
              FOR UPDATE OF r, q \
         ) \
         UPDATE run_queue AS q \
            SET attempts = q.attempts + 1, \
                lease_owner = NULL, lease_expires_at = NULL, \
                available_at = statement_timestamp() + \
                    LEAST({MAX_RETRY_BACKOFF_MS}::double precision, \
                        1000.0 * power(2.0, LEAST(q.attempts, 9)) \
                        * (1.0 + random() * 0.2)) * interval '1 millisecond' \
           FROM held WHERE q.env_key = held.env_key AND q.run_id = held.run_id \
         RETURNING q.attempts >= q.max_attempts, held.wiring_id, held.wiring_version"
    )
}

/// Lock one crash-budget-exhausted candidate for host-owned janitor handling.
///
/// `$1` is the grace period in milliseconds; `$2` is the exact package-id set
/// in the mounted release and `$3` is the environment scope.
pub fn select_exhausted_production_sql() -> String {
    format!(
        "SELECT q.env_key, q.run_id, selected_run.status, \
                selected_run.flow_id, selected_run.flow_version, \
                selected_run.wiring_id, \
                selected_run.wiring_version \
           FROM run_queue AS q \
           JOIN runs AS selected_run \
             ON selected_run.env_key = q.env_key \
            AND selected_run.run_id = q.run_id \
          WHERE q.env_key = current_setting('app.env_key', true) \
            AND selected_run.package_id = ANY($2::text[]) \
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

/// Mark one already locked exhausted run and dequeue it.
///
/// Params: run id, exact generic failure JSON, RFC 8785 body hash, terminal
/// instant. Caller outcome compare-and-set semantics match claim refusals:
/// callerless fields stay NULL and an existing released winner is preserved
/// byte-for-byte. A component-era management run stores the same truthful
/// failure as its result because it has no durable caller row for
/// reconciliation to observe.
///
/// The caller supplies the instant (D2b). Both stamps here, `caller_released_at`
/// and `updated_at`, take it. The status test beside them stays a literal
/// comparison, so no comparison leaves the server.
pub fn terminalize_exhausted_production_sql() -> String {
    terminalize_exhausted_sql(RunStatus::InfrastructureFailure, None)
}

/// Fail a locked run that spent its last attempt on a credential release.
/// Binds match [`terminalize_exhausted_production_sql`].
pub fn terminalize_released_production_sql() -> String {
    terminalize_exhausted_sql(RunStatus::Failed, Some(FailType::RetryExhausted))
}

fn terminalize_exhausted_sql(status: RunStatus, fail_type: Option<FailType>) -> String {
    let terminal_reason = fail_type.map_or(status.as_sql(), FailType::as_sql);
    let fail_type_sql = fail_type.map_or_else(
        || "r.fail_type".to_owned(),
        |kind| format!("'{}'", kind.as_sql()),
    );
    format!(
        "WITH updated AS ( \
            UPDATE runs AS r \
            SET status = '{status}', terminal_reason = '{terminal_reason}', \
                fail_type = {fail_type_sql}, \
                result_json = CASE \
                    WHEN {candidate_result} THEN $2::text::jsonb \
                    ELSE r.result_json END, \
                caller_outcome_type = CASE \
                    WHEN {unreleased_attached} THEN 'failed' \
                    ELSE r.caller_outcome_type END, \
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
                    WHEN {unreleased_attached} THEN $4::timestamptz \
                    ELSE r.caller_released_at END, \
                updated_at = $4::timestamptz \
           WHERE r.env_key = current_setting('app.env_key', true) \
             AND r.run_id = $1 \
             AND r.status IN ('{dispatched}', '{running}') \
           RETURNING r.env_key, r.run_id, r.status \
         ), \
         dequeued AS ( \
             DELETE FROM run_queue AS q USING updated \
              WHERE q.env_key = updated.env_key AND q.run_id = updated.run_id \
              RETURNING q.run_id \
         ) \
         SELECT updated.status FROM updated \
          WHERE EXISTS (SELECT 1 FROM dequeued)",
        status = status.as_sql(),
        dispatched = RunStatus::Dispatched.as_sql(),
        running = RunStatus::Running.as_sql(),
        candidate_result = if fail_type.is_some() {
            "true"
        } else {
            "r.flow_id IS NULL AND r.flow_version IS NULL \
                            AND r.wiring_hash IS NOT NULL \
                            AND r.binding_world_json IS NOT NULL"
        },
        unreleased_attached = "r.trigger_source IN ('http','internal','studio') \
                               AND r.caller_released_at IS NULL",
    )
}

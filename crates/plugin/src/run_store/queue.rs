//! Parameterized SQL for the durable global FIFO queue.
//!
//! Identifiers are static, runtime values are bound, and table names remain
//! unqualified so the host-injected transaction `search_path` selects the run
//! plane. The production lease is deliberately split into small statements
//! composed by one host-owned transaction: lock and replace the expired lease.

use crate::{FailType, RunStatus};

/// Lock the oldest expired lease for this application.
pub(super) fn select_production_lease_sql() -> &'static str {
    "SELECT q.run_id, r.status, r.input_json::text, r.graph_id, r.graph_version, \
            r.graph_hash, r.execution_principal_id, r.execution_role, \
            r.state_json::text, r.event_root_run_id, r.event_depth \
       FROM run_queue AS q JOIN run AS r USING (run_id) \
      WHERE r.package_id = $1 AND r.env = $2 \
        AND q.lease_expires_at <= clock_timestamp() AND q.attempts < q.max_attempts \
      ORDER BY q.available_at, q.run_id \
      LIMIT 1 FOR UPDATE OF r, q SKIP LOCKED"
}

/// Count an expired lease against the shared retry budget.
///
/// `$1` is the selected run id. This is deliberately its OWN statement, taken
/// BEFORE the lease grant and OUTSIDE the grant's subtransaction: the grant can
/// abort (a database guard on the queue row refuses the write), and an increment
/// that shared the aborted statement rolled back with it — so the run could
/// never reach `max_attempts`, the janitor could never reap it, and it stayed
/// the FIFO head forever (wamn-0h0g.15.69).
///
pub(super) fn advance_lease_attempts_sql() -> &'static str {
    "UPDATE run_queue SET attempts = attempts + 1 WHERE run_id = $1 RETURNING attempts"
}

/// Replace the expired lease selected under the run and queue locks.
pub(super) fn grant_production_lease_sql() -> &'static str {
    "UPDATE run_queue AS q SET lease_owner = $2, \
           lease_expires_at = clock_timestamp() + ($3::bigint * interval '1 millisecond'), \
           lease_generation = q.lease_generation + 1 \
      WHERE q.run_id = $1 RETURNING lease_generation"
}

/// Extend one live production lease under its exact owner/generation fence.
///
/// Params: run id, host-injected lease owner, lease generation, TTL
/// milliseconds. A missing row is the complete fence-lost result: the caller
/// must stop the in-flight router walk without another run-store access.
pub(super) fn renew_production_lease_sql() -> String {
    "UPDATE run_queue AS q \
        SET lease_expires_at = clock_timestamp() \
            + ($4::bigint * interval '1 millisecond') \
      WHERE q.run_id = $1 \
        AND q.lease_owner = $2 \
        AND q.lease_generation = $3 \
        AND q.lease_expires_at > clock_timestamp() \
      RETURNING q.lease_expires_at"
        .to_string()
}

/// Lock one crash-budget-exhausted candidate for host-owned janitor handling.
///
/// Bind the grace period in milliseconds, the application, and the environment.
pub(super) fn select_exhausted_production_sql() -> String {
    format!(
        "SELECT q.run_id, selected_run.status, \
                selected_run.graph_id, \
                selected_run.graph_version \
           FROM run_queue AS q \
           JOIN run AS selected_run \
             ON selected_run.run_id = q.run_id \
          WHERE selected_run.package_id = $2 \
            AND selected_run.env = $3 \
                AND q.lease_expires_at IS NOT NULL \
                AND q.lease_expires_at \
                    + ($1::bigint * interval '1 millisecond') <= now() \
                AND q.attempts >= q.max_attempts \
            AND selected_run.status IN ('{dispatched}', '{running}') \
          ORDER BY q.available_at, q.run_id \
          FOR UPDATE OF selected_run, q SKIP LOCKED \
          LIMIT 1",
        dispatched = RunStatus::Dispatched.as_sql(),
        running = RunStatus::Running.as_sql(),
    )
}

/// Mark one already locked exhausted run and dequeue it.
///
/// Params: run id, generic failure JSON, terminal
/// instant. Every exhausted run records its failure and loses its queue row.
pub(super) fn terminalize_exhausted_production_sql() -> String {
    terminalize_exhausted_sql(RunStatus::InfrastructureFailure, None)
}

fn terminalize_exhausted_sql(status: RunStatus, fail_type: Option<FailType>) -> String {
    let terminal_reason = fail_type.map_or(status.as_sql(), FailType::as_sql);
    let fail_type_sql = fail_type.map_or_else(
        || "r.fail_type".to_owned(),
        |type_| format!("'{}'", type_.as_sql()),
    );
    format!(
        "WITH updated AS ( \
            UPDATE run AS r \
            SET status = '{status}', terminal_reason = '{terminal_reason}', \
                fail_type = {fail_type_sql}, \
                result_json = $2::text::jsonb, \
                updated_at = $3::timestamptz \
           WHERE r.run_id = $1 \
             AND r.status IN ('{dispatched}', '{running}') \
           RETURNING r.run_id, r.status \
         ), \
         dequeued AS ( \
             DELETE FROM run_queue AS q USING updated \
              WHERE q.run_id = updated.run_id \
              RETURNING q.run_id \
         ) \
         SELECT updated.status FROM updated \
          WHERE EXISTS (SELECT 1 FROM dequeued)",
        status = status.as_sql(),
        dispatched = RunStatus::Dispatched.as_sql(),
        running = RunStatus::Running.as_sql(),
    )
}

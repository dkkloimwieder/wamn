//! The parameterized SQL the driver executes — pure `String` builders, the same
//! discipline as `wamn-ddl`/`wamn-api`: table/column identifiers are static
//! literals (no user input, nothing to quote), every runtime value is a `$n`
//! placeholder the driver binds, and every instant is server-side `now()` (the
//! crate reads no clock). Table names are UNQUALIFIED — resolved via the session
//! `search_path`, exactly as the flowrunner guest resolves `runs`/`node_runs`.
//!
//! The queue co-transacts with the 5.7 run row: [`write_ahead_run_sql`] +
//! [`enqueue_sql`] are the D15 write-ahead enqueue (a `dispatched` run row before
//! any work); a claim flips it to `running` ([`mark_running_sql`]); completion
//! dequeues ([`dequeue_sql`]); the janitor sweeps abandoned rows to
//! `infrastructure-failure`. Milliseconds are converted to an `interval` inline
//! (`$n::bigint * interval '1 millisecond'`).

use wamn_run_store::RunStatus;

/// The D15 write-ahead run row: a `dispatched` run persisted *before* the runner
/// picks it up (the janitor later reconciles one that never reports back).
/// Params: `$1` run_id, `$2` flow_id, `$3` flow_version. Idempotent on redelivery.
pub fn write_ahead_run_sql() -> String {
    format!(
        "INSERT INTO runs (tenant_id, run_id, flow_id, flow_version, status) \
         VALUES (current_setting('app.tenant', true), $1, $2, $3, '{dispatched}') \
         ON CONFLICT (tenant_id, run_id) DO NOTHING",
        dispatched = RunStatus::Dispatched.as_sql()
    )
}

/// Enqueue a run onto the durable queue, co-transacted with [`write_ahead_run_sql`]
/// (one durability domain, D3). Params: `$1` run_id, `$2` partition_key (nullable),
/// `$3` priority, `$4` delay_ms (0 = immediately claimable; >0 = a parked/delayed
/// wake). Idempotent: a redelivered enqueue for the same run is a no-op.
pub fn enqueue_sql() -> String {
    "INSERT INTO run_queue (tenant_id, run_id, partition_key, priority, available_at) \
     VALUES (current_setting('app.tenant', true), $1, $2, $3, \
             now() + ($4::bigint * interval '1 millisecond')) \
     ON CONFLICT (tenant_id, run_id) DO NOTHING"
        .to_string()
}

/// The `FOR UPDATE SKIP LOCKED` batch claim: atomically lease up to `limit`
/// claimable rows (visible, unleased or lease-expired, **redelivery budget not
/// spent**) for `$1` lease_owner with a `$2` lease_ttl_ms visibility timeout,
/// bumping `attempts`. `SKIP LOCKED` lets concurrent replicas claim disjoint rows
/// without blocking. The `attempts < max_attempts` guard is what lets the janitor
/// win the race for a crash-looping run: once the budget is spent the claim path
/// stops re-grabbing (and re-leasing) the row, so its lease ages out and the
/// janitor reaps it to `infrastructure-failure` — without it, each reclaim would
/// refresh the lease and the janitor window would never open. Returns each claimed
/// `run_id`, its new `attempts`, and `lease_expires_at`. `limit` is a numeric
/// literal (a `usize`, not user text).
pub fn claim_batch_sql(limit: usize) -> String {
    format!(
        "UPDATE run_queue AS q \
            SET lease_owner = $1, \
                lease_expires_at = now() + ($2::bigint * interval '1 millisecond'), \
                attempts = q.attempts + 1 \
          WHERE (q.tenant_id, q.run_id) IN ( \
              SELECT c.tenant_id, c.run_id FROM run_queue AS c \
               WHERE c.available_at <= now() \
                 AND (c.lease_expires_at IS NULL OR c.lease_expires_at <= now()) \
                 AND c.attempts < c.max_attempts \
               ORDER BY c.available_at, c.run_id \
               FOR UPDATE SKIP LOCKED \
               LIMIT {limit} \
          ) \
          RETURNING q.run_id, q.attempts, q.lease_expires_at"
    )
}

/// Flip a claimed run from the write-ahead `dispatched` pre-state to `running`.
/// Param: `$1` run_id. Guarded on `dispatched` so a reclaim of an already-running
/// run is a no-op.
pub fn mark_running_sql() -> String {
    format!(
        "UPDATE runs SET status = '{running}' \
          WHERE tenant_id = current_setting('app.tenant', true) \
            AND run_id = $1 AND status = '{dispatched}'",
        running = RunStatus::Running.as_sql(),
        dispatched = RunStatus::Dispatched.as_sql()
    )
}

/// Heartbeat: extend a held lease by `$2` ttl_ms. Only the current owner (`$3`)
/// may renew (`$1` run_id), so a reclaimed row is not resurrected by a straggler.
pub fn renew_lease_sql() -> String {
    "UPDATE run_queue \
        SET lease_expires_at = now() + ($2::bigint * interval '1 millisecond') \
      WHERE tenant_id = current_setting('app.tenant', true) \
        AND run_id = $1 AND lease_owner = $3"
        .to_string()
}

/// Remove a run's queue row on completion (the `runs` history row stays — the
/// queue is claim machinery, not audit). Param: `$1` run_id.
pub fn dequeue_sql() -> String {
    "DELETE FROM run_queue \
      WHERE tenant_id = current_setting('app.tenant', true) AND run_id = $1"
        .to_string()
}

/// Park a claimed run for a later wake (a `delay` node / backoff): push
/// `available_at` out by `$2` ms and release the lease so no replica holds it while
/// it sleeps. Param `$1` run_id. Reconciliation/doorbell picks it up at wake.
pub fn park_sql() -> String {
    "UPDATE run_queue \
        SET available_at = now() + ($2::bigint * interval '1 millisecond'), \
            lease_owner = NULL, lease_expires_at = NULL \
      WHERE tenant_id = current_setting('app.tenant', true) AND run_id = $1"
        .to_string()
}

/// The janitor sweep: in one statement, dequeue every abandoned row (lease expired
/// more than `$1` grace_ms ago and the redelivery budget spent) and mark its run
/// `infrastructure-failure`. RLS scopes both tables to the current tenant.
pub fn janitor_sweep_sql() -> String {
    format!(
        "WITH orphaned AS ( \
             DELETE FROM run_queue q \
              WHERE q.lease_expires_at IS NOT NULL \
                AND q.lease_expires_at + ($1::bigint * interval '1 millisecond') <= now() \
                AND q.attempts >= q.max_attempts \
              RETURNING q.tenant_id, q.run_id \
         ) \
         UPDATE runs r SET status = '{infra}' \
           FROM orphaned o \
          WHERE r.tenant_id = o.tenant_id AND r.run_id = o.run_id",
        infra = RunStatus::InfrastructureFailure.as_sql()
    )
}

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

/// The `FOR UPDATE SKIP LOCKED` batch claim for **unpartitioned** runs: atomically
/// lease up to `limit` claimable rows (visible, unleased or lease-expired,
/// **redelivery budget not spent**) for `$1` lease_owner with a `$2` lease_ttl_ms
/// visibility timeout, bumping `attempts`. `SKIP LOCKED` lets concurrent replicas
/// claim disjoint rows without blocking. The `attempts < max_attempts` guard is what
/// lets the janitor win the race for a crash-looping run: once the budget is spent
/// the claim path stops re-grabbing (and re-leasing) the row, so its lease ages out
/// and the janitor reaps it to `infrastructure-failure` — without it, each reclaim
/// would refresh the lease and the janitor window would never open. Returns each
/// claimed `run_id`, its new `attempts`, and `lease_expires_at`. `limit` is a
/// numeric literal (a `usize`, not user text).
///
/// The `partition_key IS NULL` guard leaves partitioned runs to the per-partition
/// ownership path ([`claim_partition_head_sql`]) so they are never dispatched out of
/// order by the order-agnostic global claim.
pub fn claim_batch_sql(limit: usize) -> String {
    format!(
        "UPDATE run_queue AS q \
            SET lease_owner = $1, \
                lease_expires_at = now() + ($2::bigint * interval '1 millisecond'), \
                attempts = q.attempts + 1 \
          WHERE (q.tenant_id, q.run_id) IN ( \
              SELECT c.tenant_id, c.run_id FROM run_queue AS c \
               WHERE c.partition_key IS NULL \
                 AND c.available_at <= now() \
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
///
/// The `r.status IN ('dispatched', 'running')` guard on the status update is the
/// completion-vs-failover race guard (checkpoint/resume on replica loss): a run a
/// second replica successfully reclaimed and drove to a terminal state — `completed`
/// above all, but also `failed`/`cancelled` — must never be relabeled
/// `infrastructure-failure` by a janitor that fires in the window between the
/// completion write and the host's dequeue. The stale queue row is still cleaned up
/// (the `DELETE` is unguarded — a terminal run has no business holding a queue row),
/// but only a still-in-flight run's *status* is reconciled. Without the guard, a
/// reclaimed-and-completed run whose fresh lease lapsed past grace would be flipped
/// back to a failure — the janitor would overwrite a genuine success.
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
          WHERE r.tenant_id = o.tenant_id AND r.run_id = o.run_id \
            AND r.status IN ('{dispatched}', '{running}')",
        infra = RunStatus::InfrastructureFailure.as_sql(),
        dispatched = RunStatus::Dispatched.as_sql(),
        running = RunStatus::Running.as_sql(),
    )
}

// ---------------------------------------------------------------------------
// Trigger dispatcher (5.14): the always-on control-plane loop that fires cron
// ticks and outbox row events into the queue and wakes parked runners. Each
// firing is the same write-ahead + enqueue co-transaction as above, with the
// trigger payload persisted (`write_ahead_triggered_run_sql`); the outbox is
// polled (D4 — LISTEN/NOTIFY removed entirely) and acked in that SAME
// transaction. See `crate::cron` / `crate::outbox` / `crate::dispatch`.
// ---------------------------------------------------------------------------

/// The dispatcher's write-ahead run row: [`write_ahead_run_sql`] plus the trigger
/// payload — `input_json` (what a replay re-runs, 5.7) and the audit
/// `trigger_source`. Params: `$1` run_id, `$2` flow_id, `$3` flow_version,
/// `$4` trigger_source, `$5` input_json as JSON **text** (`$5::text::jsonb` —
/// a bare `::jsonb` would type the param as jsonb, which the driver cannot bind
/// a string into). Idempotent on redelivery: trigger run ids are deterministic
/// (one per cron tick / outbox row), so a re-fired tick from a restarted or
/// racing dispatcher is a no-op.
pub fn write_ahead_triggered_run_sql() -> String {
    format!(
        "INSERT INTO runs (tenant_id, run_id, flow_id, flow_version, status, trigger_source, input_json) \
         VALUES (current_setting('app.tenant', true), $1, $2, $3, '{dispatched}', $4, $5::text::jsonb) \
         ON CONFLICT (tenant_id, run_id) DO NOTHING",
        dispatched = RunStatus::Dispatched.as_sql()
    )
}

/// The dispatcher's trigger-registry scan: every active flow's graph JSON. The
/// trigger lives INSIDE `graph_json` (wamn-flow `Flow.trigger`) — there is no
/// trigger column — so the driver parses each flow and registers the `cron` /
/// `row-event` ones (webhook is the gateway's, manual the editor's).
/// Tenant-scoped purely by RLS, like the claims.
pub fn active_flows_sql() -> String {
    "SELECT flow_id, version, graph_json::text AS graph_json FROM flows WHERE active".to_string()
}

/// Poll the pending outbox batch, oldest-first. `FOR UPDATE SKIP LOCKED` gives
/// two dispatcher replicas polling concurrently disjoint batches: each row stays
/// locked until its poll transaction (fire + ack) commits, and a crashed
/// poller's rows unlock and redeliver. `limit` is a numeric literal.
pub fn outbox_poll_sql(limit: usize) -> String {
    format!(
        "SELECT seq, table_name, event, payload::text AS payload FROM outbox \
          WHERE dispatched_at IS NULL \
          ORDER BY seq \
          FOR UPDATE SKIP LOCKED \
          LIMIT {limit}"
    )
}

/// Ack a polled outbox batch — CO-TRANSACTED with the write-ahead + enqueue of
/// its firings (one durability domain, D3/D4): a crash before the commit
/// redelivers the rows AND retracts their enqueues atomically, so there is no
/// half-state to reconcile. Param: `$1` seq array (`bigint[]`).
pub fn outbox_ack_sql() -> String {
    "UPDATE outbox SET dispatched_at = now() \
      WHERE tenant_id = current_setting('app.tenant', true) AND seq = ANY($1)"
        .to_string()
}

/// The PRODUCER's outbox insert — runs in the producer's own transaction (D4:
/// "outbox insert and enqueue can share a transaction with user writes"), so an
/// event is durable iff the write it announces is. In production the 3.2-emitted
/// per-table trigger or the application write path issues this; the gates issue
/// it directly. Params: `$1` table_name, `$2` event (insert|update|delete),
/// `$3` payload (JSON text, nullable).
pub fn outbox_insert_sql() -> String {
    "INSERT INTO outbox (tenant_id, table_name, event, payload) \
     VALUES (current_setting('app.tenant', true), $1, $2, $3::text::jsonb)"
        .to_string()
}

/// Recover a flow's last fired cron tick: the max run id among `$1`'s own
/// cron-sourced runs. The predicate is FLOW-EXCLUSIVE (`flow_id` +
/// `trigger_source = 'cron'`), never a lexical run-id range — flow ids are
/// unconstrained user text and `text` ordering is collation-dependent, so a
/// range scan can leak a *foreign* flow's ids into the max (a wrong anchor =
/// silently lost ticks). Within one flow's cron ids the minted ticks are
/// equal-length zero-padded digits ([`crate::mint_cron_run_id`]), so
/// `max(run_id)` IS the latest tick under any collation. The `runs` table is
/// the dispatcher's only cron state: restarted or concurrently racing
/// dispatchers recover the same anchor by construction.
pub fn cron_last_run_sql() -> String {
    "SELECT max(run_id) FROM runs \
      WHERE tenant_id = current_setting('app.tenant', true) \
        AND flow_id = $1 AND trigger_source = 'cron'"
        .to_string()
}

/// The wake / reconciliation scan: every currently-due, unleased (or
/// lease-expired), budget-remaining queue row — a parked run whose
/// `available_at` arrived, or a run whose enqueue-time doorbell hint was lost.
/// The dispatcher publishes a doorbell hint per row; a duplicate hint is
/// harmless (fire-and-forget — the claim is the arbiter), which is what lets one
/// read-only scan double as both the parked-wake and the lost-hint
/// reconciliation backstop. `limit` is a numeric literal.
pub fn parked_due_sql(limit: usize) -> String {
    format!(
        "SELECT run_id FROM run_queue \
          WHERE available_at <= now() + interval '250 milliseconds' \
            AND (lease_expires_at IS NULL OR lease_expires_at <= now()) \
            AND attempts < max_attempts \
          ORDER BY available_at, run_id \
          LIMIT {limit}"
    )
}

// ---------------------------------------------------------------------------
// Per-partition ownership (5.14 scaling): `partitioned(key)` runs dispatch
// in-order per key across replicas. A replica leases a partition (a
// `partition_owner` row) and then claims that partition's runs head-first, one in
// flight at a time; on owner death the partition lease expires and another replica
// reacquires the key and continues in order. See `crate::partition`.
// ---------------------------------------------------------------------------

/// Lease up to `limit` **acquirable** partitions to `$1` lease_owner for a `$2`
/// lease_ttl_ms: the distinct keys that have a claimable run and are not currently
/// held by a live partition lease (unowned, or the owner's lease expired = failover).
/// The `INSERT … ON CONFLICT (tenant_id, partition_key) DO UPDATE … WHERE
/// lease_expires_at <= now()` makes the `partition_owner` PK the single arbitration
/// point: two replicas racing for the same key serialize on its row, and the
/// `WHERE` only lets an **expired** lease be stolen, so exactly one wins — no
/// `FOR UPDATE` on `run_queue` needed (and `SELECT DISTINCT` forbids it anyway).
/// The `NOT EXISTS` prefilter just avoids pointlessly contending live-owned keys.
/// Returns the partitions this replica now owns. `limit` is a numeric literal.
pub fn acquire_partitions_sql(limit: usize) -> String {
    format!(
        "INSERT INTO partition_owner AS o (tenant_id, partition_key, lease_owner, lease_expires_at) \
         SELECT current_setting('app.tenant', true), cand.partition_key, $1, \
                now() + ($2::bigint * interval '1 millisecond') \
           FROM ( \
               SELECT DISTINCT q.partition_key FROM run_queue q \
                WHERE q.partition_key IS NOT NULL \
                  AND q.available_at <= now() \
                  AND (q.lease_expires_at IS NULL OR q.lease_expires_at <= now()) \
                  AND q.attempts < q.max_attempts \
                  AND NOT EXISTS ( \
                      SELECT 1 FROM partition_owner p \
                       WHERE p.tenant_id = current_setting('app.tenant', true) \
                         AND p.partition_key = q.partition_key \
                         AND p.lease_expires_at > now() \
                  ) \
                ORDER BY q.partition_key \
                LIMIT {limit} \
           ) cand \
         ON CONFLICT (tenant_id, partition_key) DO UPDATE \
             SET lease_owner = EXCLUDED.lease_owner, \
                 lease_expires_at = EXCLUDED.lease_expires_at, \
                 acquired_at = now() \
           WHERE o.lease_expires_at <= now() \
         RETURNING o.partition_key, o.lease_owner, o.lease_expires_at"
    )
}

/// Heartbeat a held partition lease: extend it by `$2` ttl_ms. Only the current
/// owner (`$3`) may renew (`$1` partition_key), so a reacquired partition is not
/// resurrected by a straggler who lost it.
pub fn renew_partition_sql() -> String {
    "UPDATE partition_owner \
        SET lease_expires_at = now() + ($2::bigint * interval '1 millisecond') \
      WHERE tenant_id = current_setting('app.tenant', true) \
        AND partition_key = $1 AND lease_owner = $3"
        .to_string()
}

/// Release a held partition lease (a drained key, or a graceful step-down). Owner-
/// guarded (`$2`) so a straggler cannot delete a lease another replica reacquired.
/// Params: `$1` partition_key, `$2` lease_owner.
pub fn release_partition_sql() -> String {
    "DELETE FROM partition_owner \
      WHERE tenant_id = current_setting('app.tenant', true) \
        AND partition_key = $1 AND lease_owner = $2"
        .to_string()
}

/// Within the partitions `$1` owns (a live `partition_owner` lease), claim the
/// **head** of each — the earliest-`(available_at, run_id)` run that is ready, has no
/// earlier ready sibling, and whose partition has **no run in flight** — leasing it
/// for `$2` ttl_ms and bumping `attempts`. Because the `NOT EXISTS` reduces each
/// partition to a single head candidate, `FOR UPDATE OF c SKIP LOCKED` is legal (no
/// `DISTINCT`) and takes the globally-earliest heads across owned partitions up to
/// `limit`. One-in-flight-per-partition + head-first is what keeps a key in order:
/// its next run becomes claimable only once the current one completes and dequeues.
/// Returns each claimed `run_id`, its `partition_key`, new `attempts`, and
/// `lease_expires_at`. `limit` is a numeric literal.
pub fn claim_partition_head_sql(limit: usize) -> String {
    format!(
        "UPDATE run_queue AS q \
            SET lease_owner = $1, \
                lease_expires_at = now() + ($2::bigint * interval '1 millisecond'), \
                attempts = q.attempts + 1 \
          WHERE (q.tenant_id, q.run_id) IN ( \
              SELECT c.tenant_id, c.run_id \
                FROM run_queue AS c \
                JOIN partition_owner AS o \
                  ON o.tenant_id = c.tenant_id AND o.partition_key = c.partition_key \
               WHERE c.partition_key IS NOT NULL \
                 AND o.lease_owner = $1 AND o.lease_expires_at > now() \
                 AND c.available_at <= now() \
                 AND (c.lease_expires_at IS NULL OR c.lease_expires_at <= now()) \
                 AND c.attempts < c.max_attempts \
                 AND NOT EXISTS ( \
                     SELECT 1 FROM run_queue b \
                      WHERE b.tenant_id = c.tenant_id \
                        AND b.partition_key = c.partition_key \
                        AND b.run_id <> c.run_id \
                        AND ( \
                            (b.lease_expires_at IS NOT NULL AND b.lease_expires_at > now()) \
                            OR (b.available_at <= now() AND b.attempts < b.max_attempts \
                                AND (b.lease_expires_at IS NULL OR b.lease_expires_at <= now()) \
                                AND (b.available_at, b.run_id) < (c.available_at, c.run_id)) \
                        ) \
                 ) \
               ORDER BY c.available_at, c.run_id \
               FOR UPDATE OF c SKIP LOCKED \
               LIMIT {limit} \
          ) \
          RETURNING q.run_id, q.partition_key, q.attempts, q.lease_expires_at"
    )
}

/// Garbage-collect partition leases with nothing left to own: an **expired** lease
/// whose partition has no remaining `run_queue` rows (the key drained, or all its
/// runs were retired). Expired leases whose partition still has runs are left for
/// reacquisition (failover), not deleted. No params.
pub fn gc_orphan_partitions_sql() -> String {
    "DELETE FROM partition_owner o \
      WHERE o.tenant_id = current_setting('app.tenant', true) \
        AND o.lease_expires_at <= now() \
        AND NOT EXISTS ( \
            SELECT 1 FROM run_queue q \
             WHERE q.tenant_id = o.tenant_id AND q.partition_key = o.partition_key \
        )"
    .to_string()
}

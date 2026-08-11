//! The parameterized SQL the driver executes — pure `String` builders, the same
//! discipline as `wamn-schema-compiler`/`wamn-api`: table/column identifiers are static
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

use super::model::PartitionPolicy;
use crate::{RunStatus, sql as run_sql};
use wamn_pg_core::Sql;

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
/// `$3` priority, `$4` delay_ms (0 = immediately claimable; >0 = a scheduled
/// queue wake). Idempotent: a redelivered enqueue for the same run is a no-op.
///
/// Writes no `partition_policy`, so the row takes the column default —
/// `blocking`, the D20 decision (choosing partitioned dispatch *is* opting into
/// ordering). A writer materializing a flow's explicit policy uses
/// [`enqueue_with_policy_sql`].
pub fn enqueue_sql() -> String {
    "INSERT INTO run_queue (tenant_id, run_id, partition_key, priority, available_at) \
     VALUES (current_setting('app.tenant', true), $1, $2, $3, \
             now() + ($4::bigint * interval '1 millisecond')) \
     ON CONFLICT (tenant_id, run_id) DO NOTHING"
        .to_string()
}

/// [`enqueue_sql`] with the flow's declared head-unavailability policy
/// materialized onto the row (D20) — the claim SQL branches on the ROW, never a
/// join back to the flow, so it stays self-contained. Params: `$1` run_id,
/// `$2` partition_key (nullable), `$3` priority, `$4` delay_ms, `$5` the policy
/// literal ([`PartitionPolicy::as_sql`], CHECK-constrained by the DDL).
pub fn enqueue_with_policy_sql() -> String {
    "INSERT INTO run_queue (tenant_id, run_id, partition_key, priority, available_at, partition_policy) \
     VALUES (current_setting('app.tenant', true), $1, $2, $3, \
             now() + ($4::bigint * interval '1 millisecond'), $5) \
     ON CONFLICT (tenant_id, run_id) DO NOTHING"
        .to_string()
}

/// The materializer's evt-run enqueue (D19 §5 / E4, l5i9.17): [`enqueue_sql`]
/// carrying the REAL CDC `stream_seq` so the numeric tiebreak the claim keys
/// order by (`(available_at, stream_seq, run_id)` and the partition orders) is
/// live for evt runs — every other writer leaves the column's 0 default and
/// stays byte-identical. An UNKEYED evt row: like [`enqueue_sql`], it writes no
/// `partition_policy` (the column default — D20; the kq0z coherence rule). The
/// keyed variant is [`enqueue_evt_with_policy_sql`]. Params: `$1` run_id
/// (minted by [`crate::queue::mint_evt_run_id`] — zero-padded, the belt to this
/// column's suspenders), `$2` partition_key (NULL here), `$3` priority,
/// `$4` delay_ms, `$5` stream_seq.
pub fn enqueue_evt_sql() -> String {
    "INSERT INTO run_queue (tenant_id, run_id, partition_key, priority, available_at, stream_seq) \
     VALUES (current_setting('app.tenant', true), $1, $2, $3, \
             now() + ($4::bigint * interval '1 millisecond'), $5) \
     ON CONFLICT (tenant_id, run_id) DO NOTHING"
        .to_string()
}

/// [`enqueue_evt_sql`] for a KEYED (strict/partitioned) evt row: the flow's
/// declared head-unavailability policy is materialized alongside the key,
/// exactly like [`enqueue_with_policy_sql`] (D20 / kq0z — policy and key stamp
/// coherently, never one without the other). Params: `$1` run_id,
/// `$2` partition_key, `$3` priority, `$4` delay_ms, `$5` stream_seq, `$6` the
/// policy literal ([`PartitionPolicy::as_sql`], CHECK-constrained by the DDL).
pub fn enqueue_evt_with_policy_sql() -> String {
    "INSERT INTO run_queue (tenant_id, run_id, partition_key, priority, available_at, stream_seq, partition_policy) \
     VALUES (current_setting('app.tenant', true), $1, $2, $3, \
             now() + ($4::bigint * interval '1 millisecond'), $5, $6) \
     ON CONFLICT (tenant_id, run_id) DO NOTHING"
        .to_string()
}

/// The `FOR UPDATE SKIP LOCKED` batch claim for **unpartitioned** runs: atomically
/// lease up to `limit` claimable rows (visible, unleased or lease-expired,
/// **redelivery budget not spent**) for `$1` lease_owner with a `$2` lease_ttl_ms
/// visibility timeout. `SKIP LOCKED` lets concurrent replicas claim disjoint rows
/// without blocking.
///
/// `attempts` counts **crash evidence only**: the `CASE` bumps it iff the claim
/// reclaims an *expired* lease — the prior owner died holding the run (it never
/// completed, queue-parked, or dequeued). The claim predicate has already established
/// expired-or-NULL, so `lease_expires_at IS NOT NULL` *is* "expired lease". A first
/// claim of a never-leased row and a queue-park→wake re-claim ([`park_sql`]
/// releases the lease) are free, so a retry loop can wait repeatedly without burning
/// redelivery budget, while a crash-loop still exhausts at `max_attempts`
/// (`max_attempts` = how many times a runner may die holding this run).
///
/// The budget guard is `attempts < max_attempts OR lease_expires_at IS NULL`. The
/// `attempts < max_attempts` half is what lets the janitor win the race for a
/// crash-looping run: once the budget is spent AND a (now-expired) lease is still
/// held, the claim path stops re-grabbing (and re-leasing) the row, so its lease ages
/// out and the janitor reaps it to `infrastructure-failure` — without it, each
/// reclaim would refresh the lease and the janitor window would never open. The
/// `lease_expires_at IS NULL` half wakes a budget-spent run whose lease was *released*
/// by a queue park (wamn-fqg.7): a NULL lease is proof the last owner was alive
/// (it intentionally returned the run to the queue), so the crash budget must not
/// exclude it — it is claimed, its
/// `attempts` unchanged (the crash-evidence `CASE` does not bump a NULL lease). Poison
/// stays terminal: a crash *after* the budget is spent leaves a non-NULL expired
/// lease, which fails both halves and falls to the janitor. Every successful
/// claim increments and returns the queue-owned `lease_generation` alongside its
/// `run_id`, new `attempts`, and `lease_expires_at`. `limit` is a numeric literal
/// (a `usize`, not user text).
///
/// The `partition_key IS NULL` guard leaves partitioned runs to the per-partition
/// ownership path ([`claim_partition_head_sql`]) so they are never dispatched out of
/// order by the order-agnostic global claim.
///
/// The locking `SELECT ... FOR UPDATE SKIP LOCKED LIMIT n` lives in a CTE fenced
/// `AS MATERIALIZED`, which is what makes the claim take **exactly** `n` rows.
/// NEITHER a `WHERE (tenant_id, run_id) IN (subquery)` NOR a plain `FROM (subquery)`
/// derived table is an evaluation fence: the planner may place the `LockRows` subplan
/// on the inner side of a nested-loop join and RE-SCAN it once per outer row, and
/// because `SKIP LOCKED` advances past already-locked rows on each re-scan a single
/// statement then leases FAR MORE than `n` rows. This is a plan-dependent over-claim
/// (the classic Postgres `UPDATE … FOR UPDATE SKIP LOCKED LIMIT` gotcha); it surfaced
/// through the `wamn:postgres` plugin's cached prepared-statement execution —
/// wamn-fqg.4's guest self-claim is the first caller to run this builder through the
/// plugin, and it intermittently leased the whole batch on a `LIMIT 1` claim (a rewrite
/// to the plain `FROM`-join did NOT fix it, which is what pinned the cause to plan-driven
/// subquery re-execution rather than the SQL shape). `AS MATERIALIZED` forces Postgres
/// to evaluate the CTE once into a tuplestore regardless of the join plan, so the lock
/// happens exactly once — the canonical SKIP-LOCKED batch-claim shape.
pub fn claim_batch_sql(limit: usize) -> String {
    // The locking SELECT lives in a CTE fenced `AS MATERIALIZED` so Postgres
    // evaluates `FOR UPDATE SKIP LOCKED LIMIT n` EXACTLY ONCE. A plain
    // `FROM (subquery)` derived table is NOT a fence: the planner may put the
    // LockRows subplan on the inner side of a nested-loop join and RESCAN it per
    // outer row, and each rescan's SKIP LOCKED advances to fresh unlocked rows —
    // so a `LIMIT 1` claim intermittently leases the WHOLE batch (plan-dependent,
    // surfaced first by the fqg.4 guest self-claim through the plugin's cached
    // prepared-statement path). MATERIALIZED forces single evaluation regardless
    // of the join plan (the canonical SKIP LOCKED job-queue claim shape).
    format!(
        "WITH claimed AS MATERIALIZED ( {cte} ) \
        UPDATE run_queue AS q \
            {set} \
           FROM claimed \
          WHERE q.tenant_id = claimed.tenant_id AND q.run_id = claimed.run_id \
          RETURNING q.run_id, q.attempts, q.lease_expires_at, q.lease_generation",
        cte = global_claim_cte(limit),
        set = CLAIM_LEASE_SET,
    )
}

/// The global (unpartitioned) claimable scan — predicate, order, fence — shared
/// VERBATIM by [`claim_batch_sql`] and [`claim_dispatch_sql`] so the two claim
/// paths cannot drift: the fqg.7 wedge-wake disjunct, the budget guard, and the
/// `FOR UPDATE SKIP LOCKED LIMIT n` all live here exactly once.
///
/// The order is `(available_at, stream_seq, run_id)` (E4): `stream_seq` is a
/// BIGINT carried AHEAD of the text `run_id`, so CDC event runs
/// (`<flow>:evt:<stream_seq>`) claim by NUMERIC stream position rather than
/// lexical id order — `f1:evt:10` must not sort before `f1:evt:9`. Inert today
/// (every enqueue writes `stream_seq = 0`, so it collapses to the prior
/// `(available_at, run_id)` order); the not-yet-built materializer supplies real
/// values.
///
/// Carries the explicit `c.tenant_id = current_setting('app.tenant', true)`
/// predicate (R8b-b) — behaviorally inert (RLS injects the identical filter, and
/// the claimable index already leads with `tenant_id`) but defense-in-depth, so
/// both claim paths match the ack/prune/insert builders instead of relying on RLS
/// alone.
fn global_claim_cte(limit: usize) -> String {
    format!(
        "SELECT c.tenant_id, c.run_id FROM run_queue AS c \
             WHERE c.tenant_id = current_setting('app.tenant', true) \
               AND c.partition_key IS NULL \
               AND c.available_at <= now() \
               AND (c.lease_expires_at IS NULL OR c.lease_expires_at <= now()) \
               AND (c.attempts < c.max_attempts OR c.lease_expires_at IS NULL) \
             ORDER BY c.available_at, c.stream_seq, c.run_id \
             FOR UPDATE SKIP LOCKED \
             LIMIT {limit}"
    )
}

/// The lease-write SET clause both claim paths apply (`$1` owner, `$2` ttl_ms):
/// attempts bump only as crash evidence — an expired prior lease (fqg.5).
const CLAIM_LEASE_SET: &str = "SET lease_owner = $1, \
                lease_expires_at = now() + ($2::bigint * interval '1 millisecond'), \
                lease_generation = q.lease_generation + 1, \
                attempts = q.attempts + CASE WHEN q.lease_expires_at IS NOT NULL THEN 1 ELSE 0 END";

/// The record-stream claim (fqg.18): ONE statement doing what the guest's claim
/// preamble previously spent three on — claim the next unpartitioned run
/// ([`claim_batch_sql`]'s exact scan + lease write, via the shared fragments),
/// flip its run `dispatched` -> `running` (the [`mark_running_sql`] guard), and
/// return the dispatch inputs (`flow_id`, execution-only input) plus the run's
/// **persisted** `flow_version` so the guest's plan cache can probe for free.
/// Params: `$1` owner, `$2` ttl_ms. Returns 0 or 1 row: `(run_id, flow_id,
/// input_json::text, flow_version)`. `flow_version` is the run's OWN column
/// (`runs.flow_version`), the version the dispatcher stamped at write-ahead
/// time (the active version THEN) — not whatever is active NOW. That is what
/// pins a resume to the version the run started under (wamn-cox): a flow edited
/// mid-run cannot make a resumed claim reconstruct against a divergent graph, and
/// a hot-reload is still picked up because newly dispatched runs carry the new
/// version. Event input gains transient causation synthesized from the trusted
/// run columns; the persisted business input remains unchanged.
pub fn claim_dispatch_sql() -> String {
    format!(
        "WITH claimed AS MATERIALIZED ( {cte} ), \
         leased AS ( \
            UPDATE run_queue AS q \
               {set} \
              FROM claimed \
             WHERE q.tenant_id = claimed.tenant_id AND q.run_id = claimed.run_id \
             RETURNING q.tenant_id, q.run_id, q.lease_generation \
         ), \
         marked AS ( \
            UPDATE runs AS r \
               SET status = '{running}' \
              FROM leased \
             WHERE r.tenant_id = leased.tenant_id AND r.run_id = leased.run_id \
               AND r.status = '{dispatched}' \
         ) \
         SELECT l.run_id, r.flow_id, ({execution_input})::text AS input_json, \
                r.flow_version AS flow_version, \
                l.lease_generation \
           FROM leased AS l \
           JOIN runs AS r ON r.tenant_id = l.tenant_id AND r.run_id = l.run_id",
        cte = global_claim_cte(1),
        set = CLAIM_LEASE_SET,
        running = RunStatus::Running.as_sql(),
        dispatched = RunStatus::Dispatched.as_sql(),
        execution_input = run_sql::execution_input_sql("r"),
    )
}

/// Begin driving one HTTP run already claimed by final admission.
///
/// Params: `$1` run id, `$2` lease owner, `$3` lease generation, `$4` lease
/// TTL milliseconds. The host-injected tenant remains authoritative. The
/// statement locks the exact run/queue pair before classification, so two
/// callers presenting the same tuple cannot both become drivers. A fresh
/// `dispatched` run is accepted immediately; a `running` run is recoverable
/// only after its prior lease expires, preserving the same owner and
/// generation. No generic availability scan or generation bump occurs.
///
/// The returned row is `(result_code, flow_id, input_json, flow_version)`.
/// `result_code` is `claimed`, `not-found`, `fence-lost`, `not-inline`,
/// `already-driven`, or `not-claimed`.
pub fn begin_claimed_run_sql() -> String {
    format!(
        "WITH authority AS MATERIALIZED ( \
             SELECT q.tenant_id, q.run_id, q.lease_owner, q.lease_generation, \
                    q.lease_expires_at, q.partition_key, r.status, r.flow_id, \
                    r.input_json::text AS input_json, r.flow_version \
               FROM run_queue AS q \
               JOIN runs AS r USING (tenant_id, run_id) \
              WHERE q.tenant_id = current_setting('app.tenant', true) \
                AND q.run_id = $1 \
              FOR UPDATE OF q, r \
         ), classified AS ( \
             SELECT CASE \
                      WHEN a.lease_owner IS DISTINCT FROM $2 \
                        OR a.lease_generation <> $3 THEN 'fence-lost' \
                      WHEN a.partition_key IS NOT NULL THEN 'not-inline' \
                      WHEN a.status = '{dispatched}' THEN 'ready' \
                      WHEN a.status = '{running}' AND a.lease_expires_at <= now() \
                        THEN 'ready' \
                      WHEN a.status = '{running}' THEN 'already-driven' \
                      ELSE 'not-claimed' \
                    END AS result_code, a.* \
               FROM authority AS a \
         ), marked AS ( \
             UPDATE runs AS r SET status = '{running}', updated_at = now() \
               FROM classified AS c \
              WHERE c.result_code = 'ready' \
                AND r.tenant_id = c.tenant_id AND r.run_id = c.run_id \
             RETURNING r.tenant_id, r.run_id \
         ), renewed AS ( \
             UPDATE run_queue AS q \
                SET lease_expires_at = now() + ($4::bigint * interval '1 millisecond') \
               FROM marked AS m \
              WHERE q.tenant_id = m.tenant_id AND q.run_id = m.run_id \
                AND q.lease_owner = $2 AND q.lease_generation = $3 \
             RETURNING q.tenant_id, q.run_id \
         ) \
         SELECT CASE WHEN n.run_id IS NOT NULL THEN 'claimed' ELSE c.result_code END, \
                c.flow_id, c.input_json, c.flow_version \
           FROM classified AS c LEFT JOIN renewed AS n USING (tenant_id, run_id) \
         UNION ALL \
         SELECT 'not-found', NULL::text, NULL::text, NULL::integer \
          WHERE NOT EXISTS (SELECT FROM authority)",
        dispatched = RunStatus::Dispatched.as_sql(),
        running = RunStatus::Running.as_sql(),
    )
}

/// Completion + dequeue as ONE statement (fqg.18): composes the 5.7
/// [`run_sql::update_run_completed`] (deliberately UNCONDITIONAL —
/// the fqg.2 reverse-race override) with [`dequeue_sql`], sharing `$1` run_id
/// (`$2` result_json). Also strictly better than the split pair: completion and
/// queue removal are now atomic, so no crash window leaves a completed run
/// enqueued. The dequeue tail shares `$1` and appends NO new param, so the
/// composed arity is exactly the head's ([`wamn_pg_core::Sql`], SR11).
///
/// SR12 (composed statement): the pure tests pin the text and the shared-bind
/// arithmetic; they cannot observe the single-statement CTE data-modification
/// semantics (both halves see the same snapshot; the dequeue does not see
/// `done`'s write) or RLS on either half — that is the SR12b live gate.
pub fn complete_dequeue_sql() -> String {
    format!(
        "WITH done AS ({completed}) {dequeue}",
        completed = run_sql::update_run_completed().text(),
        dequeue = dequeue_sql(),
    )
}

/// Per-node checkpoint + heartbeat as ONE statement (fqg.18): composes the 5.7
/// [`run_sql::insert_node_run_success`] (`$1`..`$7`, idempotent by
/// `(run_id, node_id, occurrence)`) with the [`renew_lease_sql`] write (ttl_ms,
/// owner — owner-guarded, sharing `$1` run_id). The renew fires even when the
/// record is a conflict no-op (a replay of an already-recorded visit), so a
/// long cyclic walk's lease stays live exactly as the split pair kept it.
pub fn record_success_and_renew_sql() -> String {
    checkpoint_then_renew(run_sql::insert_node_run_success())
}

/// The error-routed twin of [`record_success_and_renew_sql`]: composes
/// [`run_sql::insert_node_run_error`] (`$1`..`$8`) with the
/// owner-guarded lease renew.
pub fn record_error_and_renew_sql() -> String {
    checkpoint_then_renew(run_sql::insert_node_run_error())
}

/// Compose a per-node checkpoint `head` with the owner-guarded lease-renew tail
/// they share (fqg.18). SR11: the tail SHARES the head's `$1` (run_id) and appends
/// two NEW params — ttl_ms and the owner guard — numbered against the head's arity
/// ([`Sql::param`]), so a param added upstream (a different crate) can never
/// silently shift them onto the wrong bind. Success head arity 7 → the renew binds
/// land at `$8`/`$9`; error head arity 8 → `$9`/`$10`.
///
/// SR12 (composed statement): the pure tests pin the text and the arity
/// arithmetic; they cannot observe the data-modifying-CTE snapshot semantics,
/// the owner-guard race, or RLS on the co-transacted tables — that is the
/// SR12b live gate over the real prepared-statement path.
fn checkpoint_then_renew(head: Sql) -> String {
    format!(
        "WITH recorded AS ({insert}) \
         UPDATE run_queue \
            SET lease_expires_at = now() + (${ttl}::bigint * interval '1 millisecond') \
          WHERE tenant_id = current_setting('app.tenant', true) \
            AND run_id = $1 AND lease_owner = ${owner}",
        insert = head.text(),
        ttl = head.param(1),
        owner = head.param(2),
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

/// The TERMINAL-failure dequeue (wamn-v8cv, the D20 dead-letter + continue
/// decision): ONE statement that dequeues a run the guest watched fail
/// terminally (business failure / retries exhausted with no error route) and,
/// iff the row is the head of a **`blocking`-policy partition**, lands the
/// `run_dead_letters` marker in the SAME transaction — so strict ordering never
/// proceeds past a failed head silently, and the dequeue can never commit
/// without its ledger row. For an unpartitioned or `leapfrog` row the insert's
/// predicate matches nothing and the statement degenerates to [`dequeue_sql`]
/// (no ordering promise was made — see [`crate::queue::dead_letters_on_terminal`],
/// the pure twin of the predicate). `flow_id` rides from the run's own `runs`
/// row (the FK guarantees it); `failed_at` is server-side `now()`. Idempotent
/// on redelivery: `ON CONFLICT DO NOTHING` on the ledger PK, and a re-run
/// against an already-dequeued row inserts nothing (the SELECT is over the
/// queue row). Params: `$1` run_id (shared with the dequeue tail), `$2` the
/// failure verdict text (the `runs` row keeps the structured
/// fail_kind/fail_node/fail_reason — this is the marker's display string).
///
/// SR12 (composed statement): the pure tests pin the text, the composition, and
/// the predicate literals; they cannot observe the single-statement CTE
/// semantics (the insert's SELECT and the DELETE see the same snapshot, so the
/// insert reads the row the delete removes) or RLS on either half — that is
/// the run-queue live gate.
pub fn dead_letter_dequeue_sql() -> String {
    format!(
        "WITH dead_lettered AS ( \
             INSERT INTO run_dead_letters (tenant_id, run_id, partition_key, flow_id, reason) \
             SELECT q.tenant_id, q.run_id, q.partition_key, r.flow_id, $2 \
               FROM run_queue AS q \
               JOIN runs AS r ON r.tenant_id = q.tenant_id AND r.run_id = q.run_id \
              WHERE q.tenant_id = current_setting('app.tenant', true) \
                AND q.run_id = $1 \
                AND q.partition_key IS NOT NULL \
                AND q.partition_policy = '{blocking}' \
             ON CONFLICT (tenant_id, run_id) DO NOTHING \
        ) {dequeue}",
        blocking = PartitionPolicy::Blocking.as_sql(),
        dequeue = dequeue_sql(),
    )
}

/// Queue-park a claimed run for a later wake (for example bounded-retry backoff):
/// push `available_at` out by `$2` ms and release the lease so no replica holds it
/// while it waits. Param `$1` run_id. Reconciliation/doorbell picks it up at wake.
/// Releasing the lease (rather than letting it expire) is also what makes the wake
/// re-claim FREE: the claim's crash-evidence `CASE` sees `lease_expires_at IS NULL`
/// and leaves `attempts` alone — this queue operation is proof of life, not a
/// node-level parked state or a crash.
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
/// A **`blocking`-policy partitioned row is exempt** (D20 terminal fold-in): its
/// queue row is the only record that later runs of the key must wait, so reaping
/// it would silently release a key whose flow opted into strict ordering. The
/// exhausted head instead **wedges** the key — row kept, run status untouched —
/// until an operator intervenes (requeue or delete). Under `leapfrog` (and for
/// every unpartitioned row) the janitor verdict retires the run and releases the
/// key as before.
///
/// The `r.status IN ('dispatched', 'running')` guard on the status update is the
/// completion-vs-failover race guard (checkpoint/resume on replica loss): a run a
/// second replica successfully reclaimed and drove to a terminal state — `completed`
/// above all, but also `failed`/`effect-uncertain` — must never be relabeled
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
                AND (q.partition_key IS NULL OR q.partition_policy = '{leapfrog}') \
              RETURNING q.tenant_id, q.run_id \
         ) \
         UPDATE runs r SET status = '{infra}' \
           FROM orphaned o \
          WHERE r.tenant_id = o.tenant_id AND r.run_id = o.run_id \
            AND r.status IN ('{dispatched}', '{running}')",
        infra = RunStatus::InfrastructureFailure.as_sql(),
        dispatched = RunStatus::Dispatched.as_sql(),
        running = RunStatus::Running.as_sql(),
        leapfrog = PartitionPolicy::Leapfrog.as_sql(),
    )
}

// ---------------------------------------------------------------------------
// Trigger admission uses the same write-ahead + enqueue co-transaction as
// above, with the trigger payload persisted (`write_ahead_triggered_run_sql`).
// Row events are the D19 v3 event plane's (CDC reader → JetStream →
// materializer — the outbox path was torn down at l5i9.19).
// ---------------------------------------------------------------------------

/// The dispatcher's write-ahead run row: [`write_ahead_run_sql`] plus the trigger
/// payload — `input_json` (what a replay re-runs, 5.7) and the audit
/// `trigger_source`. Params: `$1` run_id, `$2` flow_id, `$3` flow_version,
/// `$4` trigger_source, `$5` input_json as JSON **text** (`$5::text::jsonb` —
/// a bare `::jsonb` would type the param as jsonb, which the driver cannot bind
/// a string into). Idempotent on redelivery: deterministic trigger run ids make
/// a redelivered event a no-op.
pub fn write_ahead_triggered_run_sql() -> String {
    format!(
        "INSERT INTO runs (tenant_id, run_id, flow_id, flow_version, status, trigger_source, input_json) \
         VALUES (current_setting('app.tenant', true), $1, $2, $3, '{dispatched}', $4, $5::text::jsonb) \
         ON CONFLICT (tenant_id, run_id) DO NOTHING",
        dispatched = RunStatus::Dispatched.as_sql()
    )
}

/// Acquire the chosen catalog head's key-share lock before pinned trigger
/// admission. The caller executes this statement and
/// [`admit_pinned_triggered_run_sql`] in one transaction so a publisher cannot
/// move the head between the final admission check and the run insert.
///
/// Params: `$1` catalog id, `$2` environment.
pub fn lock_pinned_trigger_catalog_head_sql() -> String {
    "SELECT lock_catalog_head(\
       NULLIF(current_setting('app.tenant', true), ''), $1, $2) \
     AS applied_catalog_version"
        .to_string()
}

/// Admit a trigger run against one exact applied immutable release artifact
/// and enqueue it in the same statement. The data-modifying CTE makes a queue
/// row depend on the freshly inserted, fully pinned run row; the returned code
/// distinguishes admission, duplicate identity, and release drift, and any
/// queue failure rolls the run insert back with the statement.
///
/// Params: `$1` run id, `$2` flow id, `$3` flow version, `$4` catalog id,
/// `$5` catalog version, `$6` environment, `$7` input JSON text,
/// `$8` suite id, `$9` case id, `$10` artifact hash,
/// `$11` platform revision.
pub fn admit_pinned_triggered_run_sql() -> String {
    format!(
        "WITH release_member AS MATERIALIZED ( \
           SELECT h.tenant_id, a.artifact_hash \
             FROM catalog.catalog_heads AS h \
             JOIN catalog.release_flows AS rf \
               ON rf.tenant_id = h.tenant_id AND rf.catalog_id = h.catalog_id \
              AND rf.catalog_version = h.applied_catalog_version \
             JOIN catalog.release_manifests AS rm \
               ON rm.tenant_id = rf.tenant_id AND rm.catalog_id = rf.catalog_id \
              AND rm.catalog_version = rf.catalog_version \
             JOIN catalog.flow_artifacts AS a \
               ON a.tenant_id = rf.tenant_id AND a.flow_id = rf.flow_id \
              AND a.flow_version = rf.flow_version \
            WHERE h.tenant_id = NULLIF(current_setting('app.tenant', true), '') \
              AND h.catalog_id = $4 AND h.applied_catalog_version = $5::int \
              AND h.environment = $6 AND rf.flow_id = $2 AND rf.flow_version = $3 \
              AND a.artifact_hash = $10 \
              AND $1 <> '' AND $8 <> '' AND $9 <> '' AND $11 <> '' \
              AND (SELECT count(*) \
                     FROM jsonb_array_elements(rm.members_json) AS member \
                    WHERE member ->> 'flow-id' = rf.flow_id \
                      AND (member ->> 'flow-version')::int = rf.flow_version \
                      AND member ->> 'artifact-hash' = a.artifact_hash) = 1 \
         ), inserted_run AS ( \
           INSERT INTO runs \
             (tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, \
              environment, status, trigger_source, input_json, invocation_context, \
              admission_context_version, platform_revision) \
           SELECT member.tenant_id, $1, $2, $3, $4, $5::int, $6, '{dispatched}', 'scenario', \
                  $7::text::jsonb, \
                  jsonb_build_object( \
                    'version', '0.1', \
                    'principal', jsonb_build_object( \
                      'tenant-id', member.tenant_id, 'environment', $6::text, \
                      'catalog-id', $4::text, 'catalog-version', $5::int, \
                      'run-id', $1::text, 'flow-id', $2::text, \
                      'flow-version', $3::int, 'artifact-digest', member.artifact_hash), \
                    'source', jsonb_build_object( \
                      'producer', 'scenario', 'suite-id', $8::text, 'case-id', $9::text)), \
                  '0.1', $11 \
             FROM release_member AS member \
           ON CONFLICT (tenant_id, run_id) DO NOTHING \
           RETURNING tenant_id, run_id \
         ), inserted_queue AS ( \
           INSERT INTO run_queue \
             (tenant_id, run_id, partition_key, priority, available_at) \
           SELECT tenant_id, run_id, NULL, 0, now() FROM inserted_run \
           RETURNING run_id \
         ) \
         SELECT CASE \
                  WHEN EXISTS (SELECT 1 FROM inserted_queue) THEN 'admitted' \
                  WHEN EXISTS (SELECT 1 FROM release_member) THEN 'duplicate' \
                  ELSE 'membership-drift' \
                END AS result_code",
        dispatched = RunStatus::Dispatched.as_sql(),
    )
}

/// Admit one stored-suite case against an exact validated draft and enqueue it
/// atomically, without consulting release membership for executable selection.
///
/// The authoritative run discriminator is `trigger_source = 'scenario-draft'`;
/// its trusted invocation source is the matching `producer = 'draft-scenario'`.
/// Every portable connection requirement must exactly equal the immutable base
/// requirement and resolve to an active generation carrying an unrevoked
/// draft-safe grant. The effect path rechecks that authority immediately before
/// network access.
///
/// Params: `$1` run id, `$2` flow id, `$3` proposed runtime/publish version,
/// `$4` catalog id, `$5` catalog version, `$6` environment, `$7` input JSON text,
/// `$8` suite id, `$9` case id, `$10` draft id, `$11` draft revision,
/// `$12` internal draft-content hash, `$13` exact ordinary draft artifact hash,
/// `$14` execution-bundle hash, `$15` immutable binding-base artifact hash,
/// `$16` source-suite flow version, `$17` validated-draft hash,
/// `$18` platform revision.
pub fn admit_pinned_draft_scenario_run_sql() -> String {
    format!(
        "WITH draft AS MATERIALIZED ( \
           SELECT d.* \
             FROM catalog.validated_flow_drafts AS d \
            WHERE d.tenant_id = NULLIF(current_setting('app.tenant', true), '') \
              AND d.draft_id = $10 AND d.draft_revision = $11 \
              AND d.draft_content_hash = $12 AND d.flow_id = $2 \
              AND d.runtime_flow_version = $3 AND d.catalog_id = $4 \
              AND d.catalog_version = $5::int AND d.environment = $6 \
              AND d.draft_artifact_hash = $13 AND d.execution_bundle_hash = $14 \
              AND d.binding_base_artifact_hash = $15 \
              AND d.suite_flow_version = $16 AND d.validated_draft_hash = $17 \
              AND $1 <> '' AND $8 <> '' AND $9 <> '' AND $18 <> '' \
         ), connection_decisions AS MATERIALIZED ( \
           SELECT requirement.value ->> 'name' AS requirement_name, \
                  (base_requirement.requirement_name IS NOT NULL \
                   AND binding.requirement_name IS NOT NULL \
                   AND instance.instance_id IS NOT NULL \
                   AND generation.generation IS NOT NULL \
                   AND grant_row.generation IS NOT NULL \
                   AND grant_row.revoked_at IS NULL) AS authorized \
             FROM draft AS d \
             CROSS JOIN LATERAL jsonb_array_elements( \
                 COALESCE(d.graph_json -> 'connection-requirements', '[]'::jsonb) \
             ) AS requirement(value) \
             LEFT JOIN catalog.connection_requirements AS base_requirement \
               ON base_requirement.tenant_id = d.tenant_id \
              AND base_requirement.artifact_hash = d.binding_base_artifact_hash \
              AND base_requirement.requirement_name = requirement.value ->> 'name' \
              AND base_requirement.requirement_json = requirement.value -> 'requirement' \
             LEFT JOIN catalog.connection_bindings AS binding \
               ON binding.tenant_id = d.tenant_id \
              AND binding.catalog_id = d.catalog_id \
              AND binding.catalog_version = d.catalog_version \
              AND binding.artifact_hash = d.binding_base_artifact_hash \
              AND binding.requirement_name = base_requirement.requirement_name \
              AND binding.environment = d.environment \
              AND binding.binding_status = 'active' \
              AND binding.validation_status = 'valid' \
             LEFT JOIN catalog.connection_instances AS instance \
               ON instance.tenant_id = binding.tenant_id \
              AND instance.environment = d.environment \
              AND instance.instance_id = binding.instance_id \
              AND instance.requirement_type \
                  = requirement.value #>> '{{requirement,descriptor,requirement-type}}' \
              AND instance.contract \
                  = requirement.value #>> '{{requirement,descriptor,contract}}' \
              AND instance.lifecycle_status = 'enabled' \
             LEFT JOIN catalog.connection_generations AS generation \
               ON generation.tenant_id = instance.tenant_id \
              AND generation.environment = instance.environment \
              AND generation.instance_id = instance.instance_id \
              AND generation.generation = instance.active_generation \
             LEFT JOIN catalog.draft_safe_connection_grants AS grant_row \
               ON grant_row.tenant_id = generation.tenant_id \
              AND grant_row.environment = generation.environment \
              AND grant_row.instance_id = generation.instance_id \
              AND grant_row.generation = generation.generation \
         ), authorized_draft AS MATERIALIZED ( \
           SELECT d.* FROM draft AS d \
            WHERE NOT EXISTS ( \
              SELECT 1 FROM connection_decisions AS decision \
               WHERE NOT decision.authorized \
            ) \
         ), inserted_run AS ( \
           INSERT INTO runs \
             (tenant_id, run_id, flow_id, flow_version, catalog_id, catalog_version, \
              environment, status, trigger_source, input_json, invocation_context, \
              admission_context_version, platform_revision) \
           SELECT d.tenant_id, $1, d.flow_id, d.runtime_flow_version, d.catalog_id, \
                  d.catalog_version, d.environment, '{dispatched}', 'scenario-draft', \
                  $7::text::jsonb, \
                  jsonb_build_object( \
                    'version', '0.1', \
                    'principal', jsonb_build_object( \
                      'tenant-id', d.tenant_id, 'environment', d.environment, \
                      'catalog-id', d.catalog_id, 'catalog-version', d.catalog_version, \
                      'run-id', $1::text, 'flow-id', d.flow_id, \
                      'flow-version', d.runtime_flow_version, \
                      'artifact-digest', d.draft_artifact_hash, \
                      'draft-id', d.draft_id, 'draft-revision', d.draft_revision, \
                      'validated-draft-hash', d.validated_draft_hash, \
                      'execution-bundle-hash', d.execution_bundle_hash, \
                      'binding-base-artifact-hash', d.binding_base_artifact_hash, \
                      'suite-flow-version', d.suite_flow_version), \
                    'source', jsonb_build_object( \
                      'producer', 'draft-scenario', 'suite-id', $8::text, \
                      'case-id', $9::text)), \
                  '0.1', $18 \
             FROM authorized_draft AS d \
           ON CONFLICT (tenant_id, run_id) DO NOTHING \
           RETURNING tenant_id, run_id \
         ), inserted_queue AS ( \
           INSERT INTO run_queue \
             (tenant_id, run_id, partition_key, priority, available_at) \
           SELECT tenant_id, run_id, NULL, 0, now() FROM inserted_run \
           RETURNING run_id \
         ) \
         SELECT CASE \
                  WHEN EXISTS (SELECT 1 FROM inserted_queue) THEN 'admitted' \
                  WHEN NOT EXISTS (SELECT 1 FROM draft) THEN 'draft-drift' \
                  WHEN NOT EXISTS (SELECT 1 FROM authorized_draft) \
                    THEN 'draft-connections-denied' \
                  ELSE 'duplicate' \
                END AS result_code",
        dispatched = RunStatus::Dispatched.as_sql(),
    )
}

/// The trigger-registry scan: every active flow's graph JSON. The trigger lives
/// INSIDE `graph_json` (wamn-flow `Flow.trigger`) — there is no trigger column.
/// Carries the explicit `tenant_id = current_setting('app.tenant', true)` predicate (R8b-b) —
/// behaviorally inert (RLS injects the identical filter) but defense-in-depth,
/// matching the ack/prune/insert builders rather than relying on RLS alone.
pub fn active_flows_sql() -> String {
    "SELECT flow_id, version, graph_json::text AS graph_json FROM flows \
      WHERE tenant_id = current_setting('app.tenant', true) AND active"
        .to_string()
}

/// The wake / reconciliation scan: every currently-due, unleased (or lease-expired)
/// **unpartitioned** queue row that the global claim would take — budget-remaining,
/// OR budget-spent with a released (NULL) lease (a queue-parked run that spent its
/// crash budget still wakes, matching `claim_batch_sql`; wamn-fqg.7). A queue row whose
/// `available_at` arrived, or a run whose enqueue-time doorbell hint was lost.
/// The dispatcher publishes a doorbell hint per row; a duplicate hint is
/// harmless (fire-and-forget — the claim is the arbiter), which is what lets one
/// read-only scan double as both the queue-wake and the lost-hint
/// reconciliation backstop. `limit` is a numeric literal. Carries the explicit
/// `tenant_id = current_setting('app.tenant', true)` predicate (R8b-b) — inert
/// (RLS injects the identical filter) but defense-in-depth, like the claim.
///
/// The `partition_key IS NULL` guard mirrors `global_claim_cte`'s (wamn-2jkm.29):
/// the wake scan surfaces exactly the rows the GLOBAL claim would take, so
/// `found_work` reflects **actionable** work. Without it, a partitioned FOLLOWER
/// behind a D20 `blocking` head is surfaced every sweep (it is due, unleased,
/// budget-remaining) yet is never claimable — `claim_partition_head_sql` holds it
/// behind the head — so a wedged head (the janitor is exempt from reaping a
/// blocking head by design) would pin the dispatcher cadence at `min` PERMANENTLY,
/// defeating zero-continuous-polling. Partitioned runs are claimed **only** through
/// the per-partition ownership path ([`acquire_partitions_sql`] +
/// [`claim_partition_head_sql`]), whose own due-scan the run-worker's `run-next`
/// drives on its independent poll-with-backoff reconcile — so excluding them here
/// does NOT strand a due partitioned queue wait (it wakes on the run-worker's own cadence,
/// the same backstop the whole system relies on for a lost doorbell hint), it only
/// stops a never-claimable partitioned row from masquerading as work.
pub fn parked_due_sql(limit: usize) -> String {
    format!(
        "SELECT run_id FROM run_queue \
          WHERE tenant_id = current_setting('app.tenant', true) \
            AND partition_key IS NULL \
            AND available_at <= now() + interval '250 milliseconds' \
            AND (lease_expires_at IS NULL OR lease_expires_at <= now()) \
            AND (attempts < max_attempts OR lease_expires_at IS NULL) \
          ORDER BY available_at, stream_seq, run_id \
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
                  AND (q.attempts < q.max_attempts OR q.lease_expires_at IS NULL) \
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
/// **head** of each — the earliest run that is ready, not blocked by a sibling
/// under the row's policy, and whose partition has **no run in flight** — leasing
/// it for `$2` ttl_ms. Because the `NOT EXISTS` reduces each partition to a single
/// head candidate, `FOR UPDATE OF c SKIP LOCKED` is legal (no `DISTINCT`) and takes
/// the globally-earliest heads across owned partitions up to `limit`. One-in-
/// flight-per-partition + head-first is what keeps a key in order: its next run
/// becomes claimable only once the current one completes and dequeues.
///
/// What "blocked by a sibling" means branches on the row's materialized
/// `partition_policy` (D20):
///
/// - **`blocking`** (the default): ANY sibling earlier in the key's *stream
///   order* — `(enqueued_at, stream_seq, run_id)`, stamped at enqueue and never
///   moved — blocks, whether it is ready, backed off, parked, or
///   budget-exhausted. A transiently-unavailable head holds its key (the Kafka
///   model), and an exhausted head **wedges** it (the janitor leaves the row;
///   see [`janitor_sweep_sql`]). The stream order deliberately ignores
///   `available_at`: a park/backoff pushes `available_at` into the future, so
///   any comparison over it would let a later run overtake — the exact
///   corruption the policy exists to forbid.
/// - **`leapfrog`** (opt-in): only an earlier *currently-ready* sibling blocks,
///   in `(available_at, stream_seq, run_id)` order — a backed-off or parked head
///   yields the key and a later ready run overtakes it until the head becomes due.
///
/// Both orders carry the numeric `stream_seq` AHEAD of the text `run_id` (E4), so
/// CDC event runs on a key advance by stream position, not lexical id order.
///
/// `attempts` counts **crash evidence only**, exactly as in [`claim_batch_sql`]: the
/// `CASE` bumps it iff this claim reclaims an *expired* lease. This matters most
/// here — a parked partitioned head is re-claimed head-first on *every* wake, so an
/// unconditional bump would burn the redelivery budget fastest on this path.
///
/// The budget guard is `attempts < max_attempts OR lease_expires_at IS NULL` on both
/// the head candidate `c` and the earlier-ready-sibling sub-check `b` (wamn-fqg.7): a
/// budget-spent head whose lease a park released (NULL) is claimable and wakes, and
/// an earlier such sibling still blocks the later head so in-order is preserved. A
/// poison head (budget spent, lease *expired* not NULL) fails the guard on both — it
/// is left to the janitor and does not block its partition's later runs.
/// Returns each claimed `run_id`, its `partition_key`, new `attempts`,
/// `lease_expires_at`, and incremented `lease_generation`. `limit` is a numeric
/// literal.
pub fn claim_partition_head_sql(limit: usize) -> String {
    // Same evaluation-fence discipline as [`claim_batch_sql`]: the locking head
    // selection lives in a CTE fenced `AS MATERIALIZED` so `FOR UPDATE OF c SKIP
    // LOCKED LIMIT n` runs EXACTLY ONCE. The prior `WHERE (tenant_id, run_id) IN
    // (subquery)` form leaves the LockRows scan re-executable per outer row, and a
    // rescan's SKIP LOCKED advances to fresh rows — leasing more than `n` heads
    // (the wamn-fqg.4 over-claim, in the per-partition builder = wamn-fqg.10). The
    // fence makes it structurally exact regardless of plan.
    format!(
        "WITH heads AS MATERIALIZED ( \
              SELECT c.tenant_id, c.run_id \
                FROM run_queue AS c \
                JOIN partition_owner AS o \
                  ON o.tenant_id = c.tenant_id AND o.partition_key = c.partition_key \
               WHERE c.partition_key IS NOT NULL \
                 AND o.lease_owner = $1 AND o.lease_expires_at > now() \
                 AND c.available_at <= now() \
                 AND (c.lease_expires_at IS NULL OR c.lease_expires_at <= now()) \
                 AND (c.attempts < c.max_attempts OR c.lease_expires_at IS NULL) \
                 AND NOT EXISTS ( \
                     SELECT 1 FROM run_queue b \
                      WHERE b.tenant_id = c.tenant_id \
                        AND b.partition_key = c.partition_key \
                        AND b.run_id <> c.run_id \
                        AND ( \
                            (b.lease_expires_at IS NOT NULL AND b.lease_expires_at > now()) \
                            OR (c.partition_policy = '{blocking}' \
                                AND (b.enqueued_at, b.stream_seq, b.run_id) < (c.enqueued_at, c.stream_seq, c.run_id)) \
                            OR (c.partition_policy = '{leapfrog}' \
                                AND b.available_at <= now() \
                                AND (b.attempts < b.max_attempts OR b.lease_expires_at IS NULL) \
                                AND (b.lease_expires_at IS NULL OR b.lease_expires_at <= now()) \
                                AND (b.available_at, b.stream_seq, b.run_id) < (c.available_at, c.stream_seq, c.run_id)) \
                        ) \
                 ) \
               ORDER BY c.available_at, c.stream_seq, c.run_id \
               FOR UPDATE OF c SKIP LOCKED \
               LIMIT {limit} \
        ) \
        UPDATE run_queue AS q \
            SET lease_owner = $1, \
                lease_expires_at = now() + ($2::bigint * interval '1 millisecond'), \
                lease_generation = q.lease_generation + 1, \
                attempts = q.attempts + CASE WHEN q.lease_expires_at IS NOT NULL THEN 1 ELSE 0 END \
           FROM heads \
          WHERE q.tenant_id = heads.tenant_id AND q.run_id = heads.run_id \
          RETURNING q.run_id, q.partition_key, q.attempts, q.lease_expires_at, \
                    q.lease_generation",
        blocking = PartitionPolicy::Blocking.as_sql(),
        leapfrog = PartitionPolicy::Leapfrog.as_sql(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// SR11: the renew tail numbers its ttl/owner params against the HEAD's arity,
    /// so the same composing code stays correct for any head — a param added
    /// upstream shifts the tail instead of misbinding it. A fake 5-param head puts
    /// them at `$6`/`$7`; a 6-param head at `$7`/`$8`; a 7-param head at `$8`/`$9`.
    #[test]
    fn renew_tail_numbers_against_head_arity() {
        let five = checkpoint_then_renew(Sql::new("INSERT $1 $2 $3 $4 $5", 5));
        assert!(five.contains("$6::bigint * interval '1 millisecond'"));
        assert!(five.contains("AND run_id = $1 AND lease_owner = $7"));
        assert!(!five.contains("$8"));

        let six = checkpoint_then_renew(Sql::new("INSERT $1 $2 $3 $4 $5 $6", 6));
        assert!(six.contains("$7::bigint * interval '1 millisecond'"));
        assert!(six.contains("AND run_id = $1 AND lease_owner = $8"));

        let seven = checkpoint_then_renew(Sql::new("INSERT $1 $2 $3 $4 $5 $6 $7", 7));
        assert!(seven.contains("$8::bigint * interval '1 millisecond'"));
        assert!(seven.contains("AND run_id = $1 AND lease_owner = $9"));
    }

    /// The real composed statements bind exactly the head's arity + the renew's two
    /// new params: success $1..$14, error $1..$15, complete $1..$2 (dequeue shares
    /// $1, adds none). Pinned against the arity the producing crate declares — the
    /// 9.6 capture columns (wamn-srb) grew the node-run heads (success 7 -> 12,
    /// error 8 -> 13) and the renew tail renumbered here automatically.
    #[test]
    fn composed_arity_flows_from_the_producing_crate() {
        assert_eq!(run_sql::insert_node_run_success().arity(), 12);
        let s = record_success_and_renew_sql();
        assert!(s.contains("AND run_id = $1 AND lease_owner = $14"));
        assert!(!s.contains("$15"));

        assert_eq!(run_sql::insert_node_run_error().arity(), 13);
        let e = record_error_and_renew_sql();
        assert!(e.contains("AND run_id = $1 AND lease_owner = $15"));
        assert!(!e.contains("$16"));

        assert_eq!(run_sql::update_run_completed().arity(), 2);
        let c = complete_dequeue_sql();
        assert!(c.contains(run_sql::update_run_completed().text()));
        assert!(c.contains(&dequeue_sql()));
    }
}

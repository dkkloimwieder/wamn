# Durable run queue & runner scaling (5.14)

The durable run queue is what makes flow dispatch **reliable and horizontally
scalable**: a run enqueued once is executed at-least-once by exactly one runner,
survives a runner crash, and is picked up with millisecond latency — the **hybrid**
of decision D3. Postgres owns durability (a `FOR UPDATE SKIP LOCKED` queue that
co-transacts with the run row), NATS-core carries fire-and-forget *doorbells* (a
hint per enqueue, backstopped by a slow reconciliation sweep for lost hints — zero
continuous polling), and a *run-claim lease* lets a second replica reclaim a dead
runner's work. It is the dispatch half of what the run store
([`wamn-run-store`](run-state.md), 5.7) made durable: where 5.7 persists *what
happened*, 5.14 governs *what runs next and who runs it*.

The split mirrors the rest of the platform: a **pure crate**
(`crates/wamn-run-queue`) holds the claim/lease/janitor/reconciliation decisions
and the parameterized SQL builders — no DB, no NATS, no clock (`now` is a passed-in
millis), unit-tested off-cluster — and the **driver** (`crates/wamn-host`
`queuebench`, and the production dispatcher) supplies the `wamn:postgres` effects
against the schema in [`deploy/run-queue.sql`](../deploy/run-queue.sql), the
NATS-core doorbell, the real clock, and the replica identity.

## The table

`run_queue` — one row per run waiting to be, or being, dispatched. It sits beside
the immutable 5.7 `runs` history (one durability domain, D3 — same DB, and the
enqueue co-transacts with the write-ahead `runs` INSERT) but is a **separate**
table: the queue row is high-churn claim/lease state that is deleted on
completion, while the `runs` row is audit history that lives forever.

| Column | Role |
|---|---|
| `tenant_id`, `run_id` | PK; FK → `wamn_run.runs` `ON DELETE CASCADE` |
| `available_at` | visibility gate — future = a delayed / parked / backed-off run |
| `lease_owner`, `lease_expires_at` | the replica currently holding the run; past expiry it is reclaimable (crash-safe failover) |
| `attempts`, `max_attempts` | redelivery budget; spent + long-expired ⇒ the janitor gives up |
| `partition_key`, `priority` | reserved for the deferred per-partition-ownership follow-up (the skeleton claims globally in `available_at` order) |

The table sits on the house tenant floor — `FORCE ROW LEVEL SECURITY` keyed on
`current_setting('app.tenant', true)`, granted to the non-owner `wamn_app` role —
so a missing claim sees zero rows. The run lifecycle stays on `runs` (5.7): the
queue reuses the two statuses 5.7 already reserved for this layer — `dispatched`
(the D15 **write-ahead** pre-state) and `infrastructure-failure` (the **janitor**
verdict) — so 5.14 adds a table but changes no existing schema.

## The dispatch lifecycle

1. **Enqueue (write-ahead, D15 default).** A `dispatched` `runs` row and a
   `run_queue` row are written in one transaction *before* any work — a run that
   never reports back is still auditable. The **reduced-audit fast path** (D15
   opt-in, policy-prohibitable) writes only the run row for direct sync dispatch.
2. **Doorbell.** The enqueue publishes a fire-and-forget hint on NATS-core. A
   subscribed runner wakes and claims — no polling. NATS-core is the least durable
   link by design, so a lost hint is not a lost run:
3. **Claim.** `claim_batch_sql` atomically leases up to *N* claimable rows (visible,
   unleased or lease-expired, **and within their redelivery budget**) for the runner
   with a visibility timeout, bumping `attempts`; `FOR UPDATE SKIP LOCKED` lets
   concurrent replicas take **disjoint** rows without blocking. A claimed run flips
   `runs.status` → `running`. The `attempts < max_attempts` guard is what lets the
   janitor win the race for a crash-looping run: once the budget is spent the claim
   path stops re-grabbing (and re-leasing) the row, so its lease ages out and step 6
   reaps it — without the guard, every reclaim would refresh the lease and the
   janitor window would never open.
4. **Heartbeat / complete.** The runner renews its lease while it works and
   dequeues the row on completion (the `runs` history stays). A `delay` node parks
   the row (push `available_at` out, release the lease) for a later wake.
5. **Reconciliation.** A slow periodic claim (30 s–5 min) backstops any lost
   doorbell hint, guaranteeing eventual pickup with zero continuous polling.
6. **Janitor.** A run whose lease expired more than a grace period ago **and**
   whose redelivery budget is spent is swept in one statement to
   `infrastructure-failure`, its queue row removed — but only if the run is still
   *in flight* (`status IN ('dispatched', 'running')`): a run a replica reclaimed
   and drove to a terminal state is never relabeled (the completion-vs-failover
   race guard, under *Checkpoint/resume on replica loss* below), though its stale
   queue row is still cleaned up.

Because each `wamn:postgres` call is its own transaction, the lease is not bound
to the node writes; claim, heartbeat, and node checkpoints are independent commits
whose at-least-once redelivery is absorbed by the existing idempotency keys (the
5.7 `runs`/`node_runs` `ON CONFLICT`, the node effects' own idempotency).

## Per-partition ownership

`partitioned(key)` flows (5.11 ordering semantics) require the runs of one key to be
dispatched **in order**, even as the runner scales horizontally. 5.14 provides the
*mechanism*: a second lease table, `partition_owner` — one row per `(tenant_id,
partition_key)` — over which a replica takes an exclusive **partition lease**. While
a replica holds a live partition lease it is the *only* one that dispatches that
key's runs, so ordering within the key is preserved; the run lifecycle and the
per-run lease stay on `run_queue`.

The two claim paths are disjoint. An **unpartitioned** run (`partition_key IS NULL`)
is claimed by the order-agnostic global `claim_batch_sql`; a **partitioned** run is
claimed *only* through the ownership path, so the global claim can never dispatch it
out of order:

1. **Acquire.** `acquire_partitions_sql` leases the distinct keys that have a
   claimable run and are not held by a live partition lease (unowned, or expired). It
   is an `INSERT … ON CONFLICT (tenant_id, partition_key) DO UPDATE … WHERE
   lease_expires_at <= now()`: the `partition_owner` primary key is the single
   arbitration point, so two replicas racing for the same key serialize on its row
   and exactly one wins — the `WHERE` lets an *expired* lease be stolen (failover) but
   never a live one. No `FOR UPDATE` on `run_queue` is needed, which also sidesteps
   Postgres forbidding `FOR UPDATE` with `SELECT DISTINCT`.
2. **Claim the head.** `claim_partition_head_sql` claims, within the partitions the
   caller owns, the **head** of each — the earliest `(available_at, run_id)` run that
   is ready, has no earlier ready sibling, and whose partition has **no run in
   flight**. The `NOT EXISTS` reduces each partition to a single head candidate, so
   `FOR UPDATE OF c SKIP LOCKED` is legal (no `DISTINCT`). *One in flight per
   partition + head-first* is what keeps a key in order: its next run is claimable
   only once the current one completes and dequeues.
3. **Renew / release.** The owner heartbeats its partition lease
   (`renew_partition_sql`, owner-guarded) while it streams the key's runs, and
   releases it (`release_partition_sql`) on a graceful step-down.
4. **Failover.** If the owner dies its partition lease expires; another replica
   reacquires the whole key (the expired-lease steal above) and continues in order,
   reclaiming the abandoned in-flight run once its own run lease has expired.
5. **GC.** `gc_orphan_partitions_sql` removes an *expired* partition lease whose key
   has drained (no `run_queue` rows left); an expired lease whose key still has runs
   is left for reacquisition, not deleted.

`partition_owner` is a coarse coordination row, not run state: it carries no run
history, is **not** FK'd to `run_queue` (a `partition_key` is not unique there), and
is garbage-collected when the key drains. It sits on the same tenant floor
(`FORCE ROW LEVEL SECURITY` on `app.tenant`).

The dispatch key is `(available_at, run_id)` — a delayed / backed-off run parks (a
later `available_at`) and waits its turn. A run that exhausts its retry budget is
retired by the janitor to `infrastructure-failure` and stops holding its partition;
whether a *terminal* failure should instead **wedge** the key (block later runs until
an operator intervenes) is an ordering **policy** decision that belongs to 5.11 —
5.14 ships the mechanism.

## Checkpoint/resume on replica loss

Failover composes the two halves 5.14 and 5.7 already built: the run-queue **lease
reclaim** (a dead runner's lease ages out and `claim_batch_sql` re-claims the row,
bumping `attempts`) and 5.7 **branch-aware reconstruction**. When a runner dies
mid-run, a second replica reclaims the run and drives the *same* flowrunner guest,
which rebuilds the outstanding frontier from `node_runs`
(`wamn_run_store::reconstruct` + `Plan::resume`) and completes. Because an effectful
node re-runs only while it is *outstanding* and its effect is idempotent
(`pg-write`'s `sink ON CONFLICT`, the `runs`/`node_runs` `ON CONFLICT`), the
killed-and-reclaimed run leaves **exactly one side effect** — the kill-mid-run
guarantee, now across a replica boundary. The guest is unchanged and queue-agnostic
(it takes a `run_id`); the host orchestrates claim → reclaim → resume.

**Completion vs. the janitor.** A run a replica reclaims on its final budget unit
(`attempts` bumped to `max_attempts`) is, the instant its fresh lease lapses past
grace, exactly the run the janitor is eligible to reap. Two host-side guards keep a
successfully-reclaimed run from being mislabeled: the host **dequeues after
completion** (a completed run's queue row is gone, out of the janitor's reach), and
`janitor_sweep_sql` relabels only a **still-in-flight** run (`status IN
('dispatched', 'running')`) — so a run that reached a terminal state (above all
`completed`) is never overwritten with `infrastructure-failure` in the window
between the completion write and the dequeue. The stale queue row is still cleaned
up; only the *status* of a terminal run is left alone. The guard lives in the pure
`wamn_run_queue` builder, so the guest stays byte-identical.

The race has a **reverse ordering** the guard cannot cover: the janitor fires while
the reclaimed run is still `running` (a slow resume whose lease lapsed past grace at
the budget boundary) and legitimately reaps it — then the resume completes anyway.
There the backstop is the runner's completion write being deliberately
**unconditional** (`UPDATE runs SET status = 'completed' WHERE run_id = …`, no
status precondition): the genuine success overrides the janitor's premature
verdict, and both orderings converge on `completed`. Each ordering is gated and
mutation-tested (`failover`/`janitor-guard` for the guard direction, `reverse-race`
for the completion-wins backstop).

For a `partitioned(key)` run the failover is the per-partition path above (the
partition lease expires, another replica reacquires the key and reclaims the
abandoned in-flight run in order). Guest-*self*-claim from the queue remains a
follow-up (fqg.4); today the host orchestrates the claim/reclaim.

## Scope (5.14) vs. siblings

This issue built the D3 hybrid queue: the SKIP LOCKED queue, the write-ahead / fast
path, single-owner leases + reclaim, the janitor, the reconciliation cadence,
**per-partition ownership** for `partitioned(key)`, and **checkpoint/resume on
replica loss** (all above), proven by `queuebench` + `failoverbench`. It
deliberately does **not** ship (tracked as follow-ups):

| Deferred | Where |
|---|---|
| The shared **cron + outbox trigger dispatcher** for all projects | 5.14 follow-up |
| Wiring the runner to **claim its own work** from the queue (guest-self-claim) | 5.14 follow-up (fqg.4) |

And it does not own: the engine walk / retry / reconstruction (5.2 + 5.7 — the
claimed run drives them); the `runs`/`node_runs` schema (5.7 — 5.14 co-transacts
and reuses the reserved statuses); per-node ordering *semantics* (5.11 — 5.14
provides the per-partition claim *mechanism*); the cancel operation (5.12); the
payload byte store (5.10). The flowrunner guest is **unchanged** — the queue is a
host-side path; wiring the runner to claim its own work from the queue is a
follow-up.

## Gates

- **`cargo test -p wamn-run-queue`** — the pure decisions + SQL shape: claim
  eligibility (Ready/Leased/Parked/Exhausted), `plan_claim` ordering + limit (and
  that it skips partitioned rows), lease liveness + renewal, janitor orphan-detection,
  reconciliation cadence, **per-partition ownership** (`plan_acquire`,
  `plan_partition_claim` head-first + one-in-flight), the SQL builders'
  `SKIP LOCKED`/tenant-scoping/`RunStatus` literals, record JSON round-trip, and
  the `deploy/run-queue.sql` drift guard — all off-cluster.
- **live-apply** (`WAMN_RUN_QUEUE_PG_URL`) — applies `run-state.sql` +
  `run-queue.sql` to a throwaway Postgres and asserts the SKIP LOCKED claim
  predicate (Ready claimed, Parked/Leased skipped, expired reclaimed), the janitor
  sweep (including the **completion-vs-failover race guard** — a `completed` run with
  a stale expired+spent queue row is *not* relabeled, a real orphan still is), tenant
  RLS isolation, the FK cascade, and — via the *real* builders through
  `PREPARE`/`EXECUTE` — the partition path: partition-lease arbitration (a second
  replica cannot steal a live-owned key), head-only claim (one in flight per key),
  and in-order advance once the head dequeues.
- **`queuebench`** — the gate of record (pure host-side `tokio_postgres` claimers
  against a superuser-provisioned ephemeral schema): the D15 dispatch SLOs
  (write-ahead p99 < 15 ms, fast-path p99 < 10 ms), SKIP LOCKED **exactly-once +
  completeness** under concurrent claimers at ~1–5k claims/s, **lease-expiry
  reclaim** (crash-safe failover), the **janitor** sweep, the **NATS-core
  doorbell** async-warm latency (p50 < 25 ms / p99 < 100 ms), and the **partition**
  mode: `partitioned(key)` runs dispatched **in order per key** across concurrent
  replicas (per-key serialization + exactly-once) plus in-order partition failover.
  Runs co-located with Postgres, no CPU limit (the S2 CFS lesson), locally and
  in-cluster.
- **`failoverbench`** — checkpoint/resume on replica loss, against a superuser-
  provisioned ephemeral schema that unions the flow tables with `run_queue`. The
  `failover` mode kills replica A mid-effect, lets its lease expire, reclaims the
  run on replica B (`attempts == 2`) and resumes the *same* unchanged flowrunner
  guest via reconstruction — asserting **exactly one** side effect, that the
  exactly-once came from **reconstruction** and not just the sink constraint
  (pg-write's `node_runs.seq == 2`: the completed prefix was skipped, not
  replayed), that `runs.status` ends `completed` (never
  `infrastructure-failure`), and that a janitor sweep fired **inside** the
  completion→dequeue window (queue row forced reap-eligible) leaves it alone. The
  `janitor-guard` mode deterministically proves the same guard on static fixtures
  (a reclaimed+completed run is not reaped; a real orphan is); the `reverse-race`
  mode proves the other ordering (the janitor reaps a still-`running` run first,
  the resume's unconditional completion write still wins). All three assertions
  are mutation-tested. Same co-located, no-CPU-limit topology as `queuebench`,
  locally and in-cluster.
- **`flowbench` (S3) + `testhostbench` (S6)** — regression: the flowrunner guest is
  unchanged, so both stay green.

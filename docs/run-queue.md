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
(`crates/wamn-run-queue`) holds the claim/lease/janitor/reconciliation decisions —
and the trigger dispatcher's: cron due-tick evaluation, outbox matching,
deterministic run-id minting, the adaptive poll cadence — plus the parameterized
SQL builders — no DB, no NATS, no clock (`now` is a passed-in millis), unit-tested
off-cluster — and the **driver** (`crates/wamn-gates` `queuebench`/`dispatchbench`,
and the production `dispatch` service) supplies the `wamn:postgres` effects
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
| `attempts`, `max_attempts` | redelivery budget — `attempts` counts **crash evidence** (expired-lease reclaims) only; spent + long-expired ⇒ the janitor gives up |
| `partition_key`, `priority` | the per-partition-ownership dispatch key (see *Per-partition ownership*); `priority` remains reserved |

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
   with a visibility timeout; `FOR UPDATE SKIP LOCKED` lets concurrent replicas take
   **disjoint** rows without blocking. A claimed run flips `runs.status` →
   `running`. `attempts` counts **crash evidence only**: a claim bumps it iff it
   reclaims an *expired* lease — the prior owner died holding the run (it never
   completed, parked, or dequeued). The first dispatch is free (a first-dispatch
   crash costs its unit on the *reclaim*), and a park→wake re-claim is free (park
   releases the lease — parking is proof of life), so `max_attempts` means "how many
   times may a runner die holding this run": a delay-loop flow parks unboundedly
   without spending budget while a crash-loop still exhausts. Both claim paths
   (the global claim and the partition head claim) apply the same rule. The budget
   guard is `attempts < max_attempts OR lease_expires_at IS NULL`. The
   `attempts < max_attempts` half is what lets the
   janitor win the race for a crash-looping run: once the budget is spent **and a
   (now-expired) lease is still held**, the claim
   path stops re-grabbing (and re-leasing) the row, so its lease ages out and step 6
   reaps it — without it, every reclaim would refresh the lease and the
   janitor window would never open. The `lease_expires_at IS NULL` half closes the
   wamn-fqg.7 wedge: a budget-spent run whose lease was *released* by a park (a
   NULL lease is proof the last owner was alive — it parked, it did not crash) still
   **wakes and completes** rather than sitting invisible to claim, wake, and janitor
   alike; waking it costs no budget (the crash-evidence bump skips a NULL lease).
   Poison stays terminal: a crash *after* the budget is spent leaves a non-NULL
   expired lease, which fails both halves and falls to the janitor.
4. **Heartbeat / complete.** The runner renews its lease while it works and
   dequeues the row on completion (the `runs` history stays). A `delay` node parks
   the row (push `available_at` out, release the lease) for a later wake — without
   consuming redelivery budget.
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
   reclaiming the abandoned in-flight run once its own run lease has expired. The
   effective failover latency for a partitioned key is therefore
   `max(partition-lease TTL, run-lease TTL)`: the successor must wait out the
   *partition* lease to reacquire the key **and** the abandoned run's lease to
   reclaim its in-flight run, so tuning only one TTL does not speed failover up.
5. **GC.** `gc_orphan_partitions_sql` removes an *expired* partition lease whose key
   has drained (no `run_queue` rows left); an expired lease whose key still has runs
   is left for reacquisition, not deleted.

`partition_owner` is a coarse coordination row, not run state: it carries no run
history, is **not** FK'd to `run_queue` (a `partition_key` is not unique there), and
is garbage-collected when the key drains. It sits on the same tenant floor
(`FORCE ROW LEVEL SECURITY` on `app.tenant`).

The dispatch key is `(available_at, run_id)`, but in-order dispatch holds only
among the runs of a key that are **currently ready**: `claim_partition_head_sql`'s
no-earlier-ready-sibling predicate ranks only ready siblings, so a parked or
backed-off earlier run (a future `available_at`) **yields the head** and a
later-but-ready run of the same key overtakes it, until the earlier one becomes due.
Strict in-order-under-retry/park — whether an earlier not-yet-due run should *hold*
the key rather than yield it — is an ordering **policy** deferred to 5.11 (wamn-1d4);
5.14 ships only the mechanism. A run that exhausts its retry budget is
retired by the janitor to `infrastructure-failure` and stops holding its partition;
whether a *terminal* failure should instead **wedge** the key (block later runs until
an operator intervenes) is an ordering **policy** decision that belongs to 5.11 —
5.14 ships the mechanism.

## Checkpoint/resume on replica loss

Failover composes the two halves 5.14 and 5.7 already built: the run-queue **lease
reclaim** (a dead runner's lease ages out and `claim_batch_sql` re-claims the row,
counting one unit of crash evidence on `attempts`) and 5.7 **branch-aware
reconstruction**. When a runner dies
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

## Trigger dispatcher (cron + outbox + parked-wake)

The **shared trigger dispatcher** is the always-on control-plane loop that turns
*time* and *data changes* into runs: it owns **cron schedules** (flows with a
`cron` trigger, F3) and **outbox polling** (flows with a `row-event` trigger —
D4: LISTEN/NOTIFY is removed entirely, events are durable outbox rows the
dispatcher polls, F4) across **all projects**, and **wakes parked runners**
whose `available_at` has arrived — one service, per-project connections (D3:
reconciliation follows connection ownership, no cross-DB sweep), adaptive
intervals, no per-project listeners, no polling herd. It is the second driver of
this crate: the decisions — cron due-tick evaluation, outbox matching, run-id
minting, the poll cadence — live in the pure `cron`/`outbox`/`dispatch` modules
and take an injected `now`, so the `dispatchbench` gate fast-forwards a nightly
cron and a three-day outage in milliseconds (the 11.1 fast-forwardable-cron
discipline); `wamn-host dispatch` supplies the real clock.

One sweep of one project:

1. **Registry.** Scan active flows: the trigger lives *inside* `graph_json`
   (wamn-flow `Flow.trigger`) — `cron` and `row-event` register here; `webhook`
   is the gateway's, `manual` the editor's. A flow that fails to parse or
   validate is skipped with a warning, never wedging the project's dispatch —
   but if its trigger is still readable at the JSON level as a row event, that
   `(table, event)` is **held**: its outbox rows stay pending rather than being
   consumed, so a version-skewed flow (an older dispatcher binary meeting a
   newer flow schema) degrades to *delayed* delivery, never silent event loss.
   A row whose `flows.flow_id` **column** differs from the graph's validated
   `flow-id` is treated the same way (skipped, held if a row event): run ids
   are minted from the column, so the equality requirement extends the 5.1
   flow-id slug charset — which `validate()` enforces only on the graph field
   — to the id that is actually embedded in `{flow}:cron:{tick}` /
   `{flow}:outbox:{seq}`.
2. **Cron.** The due tick is the *latest* scheduled tick since the anchor —
   misfire collapse: an outage fires the latest missed tick once, never a
   burst. The anchor is recovered from the run ids themselves,
   **flow-exclusively**: `cron_last_run_sql` takes `max(run_id)` over the
   flow's *own* cron runs (`flow_id = $1 AND trigger_source = 'cron'`) — never
   a lexical run-id range, because flow ids are unconstrained user text and
   `text` ordering is collation-dependent, so a range scan can leak a
   *foreign* flow's ids into the max (a wrong anchor = silently lost ticks).
   Within one flow the minted ticks are equal-length zero-padded digits, so
   the max *is* the last fired tick (`cron_tick_of` parses it back by
   exact-prefix strip) — the `runs` table is the dispatcher's only cron state,
   and restarted or racing replicas agree by construction. A never-fired flow
   starts from dispatcher-sight (no retroactive catch-up). A fire is the D15
   write-ahead co-transaction with the trigger payload persisted
   (`write_ahead_triggered_run_sql` — `input_json` is what a replay re-runs,
   `trigger_source` the audit tag) + the enqueue + a doorbell hint. Schedules
   are classic cron (5-field, optional leading seconds field) evaluated in
   **UTC** (croner); per-project timezones are a later refinement. An
   **unsatisfiable schedule** (e.g. `0 0 30 2 *` — Feb 30 never comes) is an
   *error*, not a silent no-op: the schedule is quarantined per project
   (warned once, skipped, excluded from the cron-aware sleep) so it can
   neither wedge the sweep nor burn a full croner horizon walk every tick.
3. **Outbox.** Poll pending rows oldest-first (`outbox_poll_sql`, `FOR UPDATE
   SKIP LOCKED` — racing replicas take disjoint batches), **re-read the
   registry inside the same transaction, strictly after the poll** (a flow
   whose activation committed before a polled event's commit is always visible,
   so an event can never be consumed as unmatched merely because it landed
   after the sweep's first registry read — the flow-activation race), fire one
   run per (registered flow × row) with the row payload spliced into the run
   input **verbatim** (raw JSON, never a float-lossy parse/re-serialize — the
   platform's no-float rule survives into `input_json`), and ack everything not
   held (`plan_ack` + `outbox_ack_sql`) — **poll, fire, and ack in one
   transaction**: a crash anywhere before the commit redelivers the batch and
   retracts its enqueues atomically, and the deterministic ids
   (`{flow}:outbox:{seq}`) collapse the redelivery to no-ops. A row no flow is
   registered on is acked as consumed-with-no-op (an unmatched backlog must not
   pin the oldest-first poll window); a **held** row (step 1) redelivers. The
   **producer** writes outbox rows in its *own* transaction — D4's "outbox
   insert and enqueue can share a transaction with user writes" — so an event
   is durable iff the write it announces is. In production the producers are
   the 3.2-emitted per-table row triggers (`Migration::outbox_triggers`,
   `docs/ddl-compiler.md` § outbox triggers): every generated entity table
   fires `insert|update|delete` events carrying the row as jsonb payload,
   inside the user's transaction; applications can also insert directly
   (`outbox_insert_sql`). The table lives in
   [`deploy/run-queue.sql`](../deploy/run-queue.sql) beside `run_queue`.
   (`seq` is the poll's oldest-first order, not a
   cross-replica dispatch-order guarantee — per-key ordering is the 5.11
   `partition_key` seam.)

   > **Renaming a table silently stops its row-event flows.** Matching is by
   > **table name**: the producer trigger follows a rename and emits the *new*
   > `table_name`, but a `row-event` flow declares the *old* name and `match_outbox`
   > compares them for equality — so after a rename, the flow no longer matches and
   > its now-unmatched rows are **acked (consumed), not held**, so it silently stops
   > firing until re-pointed at the new name. This is the named 11.8 schema-impact
   > case (wamn-wvb); until it lands, re-point row-event flows as part of any table
   > rename.
4. **Wake / reconciliation.** One read-only scan (`parked_due_sql`) surfaces
   every currently-due, unleased, budget-remaining queue row — a parked `delay`
   wake, or a run whose enqueue-time hint was lost — and hints each on the
   doorbell. Duplicate hints are harmless by design (the claim is the arbiter),
   which is what lets one scan double as the lost-hint reconciliation backstop
   of lifecycle step 5.
5. **Cadence.** Per-project adaptive interval (`next_interval`): work tightens
   it to `min` (default 250 ms), idleness decays it exponentially to `max`
   (default 30 s — the reconciliation band's floor). The loop sleeps until the
   earliest next sweep OR the earliest upcoming cron fire across projects, so an
   idle project costs one cheap scan per `max` while a cron tick is never late
   by a decayed interval — zero continuous polling, no herd.

**Doorbell convention.** Hints are published on **`wamn.doorbell.{tenant}`**
(NATS-core, fire-and-forget), payload = the run id — the subject the queuebench
doorbell gate established. The dispatcher also runs *without* NATS (hints
skipped, the reconciliation scan still guarantees pickup): a missing broker
costs latency, not correctness.

**Exactly-once without a leader.** Run ids are deterministic per firing — one
run per (flow, cron tick) and per (flow, outbox row) — and both write-ahead and
enqueue are `ON CONFLICT DO NOTHING`, so a restarted dispatcher, a crashed
poll, and two replicas racing the same tick all collapse onto one run; HA needs
no election (the dispatchbench `race` mode runs two live dispatchers over one
project and asserts it, with the contention itself proven — the losing
attempts are counted). A cron tick's identity is its *scheduled* instant
truncated to the second, so replicas observing the same tick at different
sub-second offsets mint the same id. A firing that **loses** the write-ahead
skips its enqueue too: the winner's queue row was created in the winner's own
transaction and is either still pending or was legitimately dequeued on
completion — re-inserting it would resurrect a terminal run's queue row (a
ghost dispatch).

**Always-on hardening.** A dropped project connection is re-dialed on the next
sweep (a Postgres restart must not permanently silence a project's triggers);
every sweep runs under a deadline so a black-holed connection cannot wedge the
other projects; and a failing sweep decays that project's cadence and clears
its stale cron wake-hint (a past hint would otherwise pin the loop hot against
a down DB — the durable anchor re-fires the tick exactly once on the next
successful sweep). A failing project never wedges the loop: its errors log and
decay while every other project keeps its own cadence.

Deployment shape: `wamn-host dispatch --projects-file <json>` (one entry per
project: `url` + `tenant` + `schema` — the 2.2 projects-file pattern) or the
single-project flags. The production manifest is `deploy/dispatcher.yaml`: a
2-replica Deployment (no leader — replicas race and collapse on the write-ahead
`ON CONFLICT`, the dispatchbench `race` gate; scale-out is safe by the same
argument) + a PodDisruptionBudget, with the projects file mounted from the
`wamn-dispatch-projects` Secret and the doorbell's mTLS NATS material from
`wasmcloud-runtime-tls` (the queuebench-job pattern; a publish-only NATS
identity is a tracked follow-up — the runtime cert maps to an
allow-all-subjects user). The Secret is deliberately a SEPARATE manifest
(`deploy/dispatcher-projects.example.yaml`, demo values pointing at the
`wamn_dispatch_demo` schema — production run-state.sql + run-queue.sql objects
plus the `flows` registry, whose production DDL is now `deploy/flows.sql`
(POC-F1; applied per-project by `publish-catalog --runstate`), provisioned
additively): re-applying the
Deployment must not clobber customized project entries, and real per-project
entries land with hosting/2.3 provisioning. Shutdown: the dispatcher handles
SIGTERM explicitly (PID 1 gets no default disposition) and exits in
milliseconds; even SIGKILL mid-sweep is safe — a sweep is one transaction, so
abrupt death rolls back and redelivers. Rollouts are guarded without a
readiness endpoint (`maxUnavailable: 0` + `minReadySeconds`): a new pod with a
bad Secret crashes inside its fatal-dial timeout and stalls the rollout
instead of replacing healthy replicas. Cron anchor recovery at production
`runs`-table scale is served by the partial index `runs_cron_anchor` on
`runs (tenant_id, flow_id, run_id) WHERE trigger_source = 'cron'`
(deploy/run-state.sql) — an index-only backward scan for `cron_last_run_sql`'s
per-flow `max(run_id)` instead of a seq scan. Proven live in-cluster
(2026-07-12): two replicas against the demo project minted 4 consecutive 20s
cron ticks exactly once each, fired + acked the seeded outbox row with its
payload spliced verbatim, `EXPLAIN` confirmed the anchor query uses
`runs_cron_anchor`, and SIGTERM shutdown measured 13 ms.

## Scope (5.14) vs. siblings

This issue built the D3 hybrid queue: the SKIP LOCKED queue, the write-ahead / fast
path, single-owner leases + reclaim, the janitor, the reconciliation cadence,
**per-partition ownership** for `partitioned(key)`, **checkpoint/resume on
replica loss**, and the **shared trigger dispatcher** (cron + outbox +
parked-wake, all above), proven by `queuebench` + `failoverbench` +
`dispatchbench`. It deliberately does **not** ship (tracked as follow-ups):

| Deferred | Where |
|---|---|
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
  eligibility (Ready/Leased/Parked/Exhausted — incl. the wamn-fqg.7 rule that a
  budget-spent row wakes iff its lease was released, not merely expired),
  `plan_claim` ordering + limit (and
  that it skips partitioned rows), lease liveness + renewal, janitor orphan-detection,
  reconciliation cadence, **per-partition ownership** (`plan_acquire`,
  `plan_partition_claim` head-first + one-in-flight), the **dispatcher decisions**
  (cron `next_fire`/`due_tick` incl. leap-day/short-month calendar edges,
  sub-second tick canonicalization, misfire collapse; outbox matching + firing
  envelopes with the payload spliced **verbatim** — no-float fidelity for
  >2^53 ints and long decimals; ack planning incl. held rows; deterministic
  run-id minting + ordering; the adaptive interval), the
  SQL builders' `SKIP LOCKED`/tenant-scoping/`RunStatus` literals/`::text::jsonb`
  binding, record JSON round-trip, and the `deploy/run-queue.sql` drift guards
  (queue + partition + outbox) — all off-cluster.
- **live-apply** (`WAMN_RUN_QUEUE_PG_URL`) — applies `run-state.sql` +
  `run-queue.sql` to a throwaway Postgres and asserts the SKIP LOCKED claim
  predicate (Ready claimed, Parked/Leased skipped, expired reclaimed), the
  **crash-evidence `attempts` rule** on both claim paths (a never-leased row and a
  park→wake re-claim through the real `park_sql` claim for free; an expired-lease
  reclaim bumps), the **wamn-fqg.7 wedge** (a budget-spent NULL-lease row is
  surfaced by the wake scan, claimed with `attempts` unchanged, and left in flight by
  the janitor, while a budget-spent expired-lease row is not claimed/woken and is
  reaped — on both the global and partition-head paths), the janitor
  sweep (including the **completion-vs-failover race guard** — a `completed` run with
  a stale expired+spent queue row is *not* relabeled, a real orphan still is), tenant
  RLS isolation, the FK cascade, and — via the *real* builders through
  `PREPARE`/`EXECUTE` — the partition path: partition-lease arbitration (a second
  replica cannot steal a live-owned key), head-only claim (one in flight per key),
  and in-order advance once the head dequeues; plus the dispatcher's outbox path:
  producer insert → RLS-scoped oldest-first poll → co-transacted fire + ack, the
  **crash-rollback atomicity** (rollback = row redelivered AND enqueue retracted,
  no half-state; the redelivery dedupes on the deterministic id), cron
  last-tick recovery (flow-exclusive `max(run_id)` — cross-flow poison ids,
  including a nested `{flow}:cron:` prefix in a *neighboring* flow id, never
  leak into the anchor), and the wake scan
  (due-unleased surfaced; future/leased not).
- **`queuebench`** — the gate of record (pure host-side `tokio_postgres` claimers
  against a superuser-provisioned ephemeral schema): the D15 dispatch SLOs
  (write-ahead p99 < 15 ms, fast-path p99 < 10 ms), SKIP LOCKED **exactly-once +
  completeness** under concurrent claimers at ~1–5k claims/s, **lease-expiry
  reclaim** (crash-safe failover), the **park** mode (park/wake budget-neutrality:
  a flow parking 10× with `max_attempts = 3` completes on **both** claim paths
  with `attempts` still 0 and a janitor sweep retires nothing — the wamn-fqg.5
  regression — plus the wamn-fqg.7 corollary: a budget-spent run whose lease a park
  released **wakes and completes** on both claim paths, while a budget-spent run
  holding an expired lease is not claimed and is reaped by the janitor), the
  **janitor** sweep, the **NATS-core
  doorbell** async-warm latency (p50 < 25 ms / p99 < 100 ms), and the **partition**
  mode: `partitioned(key)` runs dispatched **in order per key** across concurrent
  replicas (per-key serialization + exactly-once) plus in-order partition failover.
  Runs co-located with Postgres, no CPU limit (the S2 CFS lesson), locally and
  in-cluster.
- **`failoverbench`** — checkpoint/resume on replica loss, against a superuser-
  provisioned ephemeral schema that unions the flow tables with `run_queue`. The
  `failover` mode kills replica A mid-effect, lets its lease expire, reclaims the
  run on replica B (`attempts == 1` — A's first claim was free, its death is the
  first counted crash) and resumes the *same* unchanged flowrunner
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
- **`dispatchbench`** — the trigger dispatcher's gate of record (pure host-side,
  no wasm guest), driving the *real* `Dispatcher` engine with **stepped time**
  against two superuser-provisioned ephemeral project schemas. `cron`: a nightly
  F3-shaped schedule fires exactly once per due tick — not early, once within a
  tick's second, no duplicate across a dispatcher **restart** (anchor recovered
  from the run ids), **misfire collapse** after a simulated three-day outage,
  first-sight bootstrap, and the fire's write-ahead + enqueue co-transaction
  proven atomic by an enqueue-side trap. `outbox`: one run per (matching flow ×
  row) with the payload persisted as the run input, unmatched rows consumed, a
  version-skewed flow's rows **held** (never wedging the sweep — junk/webhook
  registry rows are also seeded), a redelivered row deduped on the
  deterministic id **without resurrecting a completed run's queue row** (the
  ghost-dispatch guard), and the poll/fire/ack co-transaction proven atomic by
  traps on **both** sides (an ack-side trap kills the fire-first split-txn
  mutant; a fire-side trap kills the ack-first lost-event mutant). `race`:
  **two live dispatchers ticking concurrently** over one project — every cron
  tick and outbox row still fires exactly once (won-insert count == distinct
  runs) with the contention itself proven (losing attempts counted, both
  replicas must win work), no leader. `fairness`: a 120-row backlog in project
  A is batch-bounded per sweep **oldest-first** and does not starve project B's
  first sweep; the adaptive intervals tighten/decay per project independently.
  `wake`: a parked run is doorbell-hinted only once due; a firing's hint
  carries the won run id and arrives only after its transaction committed.
  `live`: the real `dispatch` run loop fires an outbox insert sub-500ms
  **beside a permanently failing project** (isolation), survives its DB
  connections being killed (reconnect), and honors the cron-aware sleep (a
  tick under a fixed 5 s interval fires within ~1 s). Every load-bearing check
  is mutation-tested (an observed-now tick identity, an ack-first split
  transaction, an unconditional enqueue, and a cron-blind sleep each fail the
  gate). Same co-located, no-CPU-limit topology as `queuebench`, locally and
  in-cluster.
- **`flowbench` (S3) + `testhostbench` (S6)** — regression: the flowrunner guest is
  unchanged, so both stay green.

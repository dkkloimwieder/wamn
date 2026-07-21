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
and the trigger dispatcher's: cron due-tick evaluation,
deterministic run-id minting, the adaptive poll cadence — plus the parameterized
SQL builders — no DB, no NATS, no clock (`now` is a passed-in millis), unit-tested
off-cluster — and the **driver** (`crates/wamn-gates` `queuebench`/`dispatchbench`,
and the production `dispatch` service) supplies the `wamn:postgres` effects
against the schema in [`deploy/sql/run-queue.sql`](../deploy/sql/run-queue.sql), the
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
| `partition_key`, `priority` | the per-partition-ownership dispatch key, stamped at fire() from the flow's ordering declaration (see *Flow-level ordering declaration* + *Per-partition ownership*); NULL = unordered; `priority` remains reserved |

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

### Flow-level ordering declaration (5.11 / wamn-fqg.20)

Which stream a run joins is declared **on the flow** (`Flow::ordering`, `wamn-flow`),
and the **dispatcher stamps `run_queue.partition_key` at fire()** from that
declaration — the `partition_policy` above is materialized the same way, so a
partitioned row is self-describing and the two claim paths never join back to the
flow. Three modes:

- **`unordered` (default).** `partition_key` stays NULL — the global claim, in
  `available_at` order. Absent field = unordered, so existing flows are unchanged.
- **`strict`.** Every run of the flow carries a **constant** key — the flow id —
  so the whole flow is one ordered stream (one run in flight, in order, across
  replicas). The key is per-flow, so two strict flows never share a stream.
- **`partitioned(partition-key)`.** The key is a JMESPath (`partition-key`) over
  the run input, evaluated at fire(). A scalar result (string / number / bool)
  is the key; a **null / missing / non-scalar** result **falls back to the flow
  id** (the flow-wide stream) rather than NULL — a flow that opted into ordering
  must never have a run silently escape to the unordered global claim (the D20
  blocking coherence: NULL = unordered dispatch, which for a partitioned flow
  would reorder its stream). The JMESPath is validated for syntactic
  well-formedness when the flow is validated (`wamn-flow`), so a mis-authored key
  fails validation rather than degrading silently at fire().

The CDC materializer (wamn-l5i9.17) consumes the **same** declaration to key its
event runs; the guest-side partitioned claim is wamn-fqg.9.

**Per-node ordering is deferred.** The 5.11 plan wording is per-node
`strict`/`partitioned`/`unordered`, but the queue's dispatch unit is the **run**
(records map 1:1 to runs, D9), so ordering is declared on the flow, not per node.
Per-node streams would be a later refinement (a distinct `partition_key` seam per
node); nothing in the current model needs them.

### Head-unavailability policy (5.11 / D20)

What a key does while its earliest (head) run is **unavailable** — backed off,
parked, or budget-exhausted — is a per-flow **policy** the flow declares
(`Flow::partition_policy`, `wamn-flow`) and the enqueue writer **materializes onto
the queue row** (`run_queue.partition_policy`), so `claim_partition_head_sql`
branches on the row alone and never joins back to the flow. The default is
**`blocking`**; `leapfrog` is the explicit opt-out. Choosing partitioned dispatch
*is* opting into ordering.

- **`blocking` (default).** ANY sibling earlier in the key's **stream order** —
  `(enqueued_at, run_id)`, stamped once at enqueue and never moved — blocks the
  head, whether it is ready, backed off, parked, or budget-exhausted. A
  transiently-unavailable head therefore *holds* its key (the Kafka-consumer
  model: a partition never leapfrogs a retrying message), and a head that exhausts
  its redelivery budget **wedges** the key: the janitor is **exempt** from reaping
  a blocking-policy row (`janitor_sweep_sql`'s
  `partition_key IS NULL OR partition_policy = 'leapfrog'` guard), so the row stays
  and later runs wait until an operator clears the head (requeue or delete). The
  stream order deliberately ignores `available_at`: a park/backoff pushes
  `available_at` into the future, so ranking over it would let a later run overtake
  — the exact corruption (consume-before-produce genealogy, state-machine streams)
  the policy exists to forbid on a transient network blip.
- **`leapfrog` (opt-in).** Only an earlier *currently-ready* sibling blocks, in
  `(available_at, run_id)` order — a backed-off or parked head yields the key and a
  later ready run overtakes it until the head becomes due, and the janitor's
  `infrastructure-failure` verdict on an exhausted head **releases** the key. For
  keys where ordering is a throughput heuristic, not a correctness requirement.

The dispatch key is still `(available_at, run_id)` — it decides *which* claimable
head is globally earliest across owned partitions; the policy decides *whether* a
key has a claimable head at all. This is inert on unpartitioned rows (the global
`claim_batch_sql` and the janitor treat `partition_key IS NULL` as always
reapable).

### Terminal business failure of a blocking head (wamn-v8cv)

The wedge above covers **crash exhaustion** — the janitor's verdict on a head
nobody could finish driving. A head the runner *did* finish driving to a
**terminal failure** (a business failure with no error route, retries exhausted,
an invalid input — the guest watched the run die) is the other terminal path,
and D20's wedge would be the wrong answer there: no redelivery will ever fix it,
so wedging trades the key's availability for a failure that needs a human
anyway. The owner-decided contract (2026-07-20) is **dead-letter + continue**:
the runner dequeues the failed head so the key continues in order, and the SAME
statement/transaction (`dead_letter_dequeue_sql`) inserts one row into
`wamn_run.run_dead_letters` — tenant, run id, partition key, flow, the failure
verdict, `failed_at` — as the alertable marker that strict ordering proceeded
past a failure. The dequeue can never commit without its marker. Scope is
**`blocking`-partitioned rows only**: an unpartitioned or `leapfrog` terminal
dequeue made no strict-ordering promise and degenerates to the plain dequeue
(the pure twin is `dead_letters_on_terminal`). The run's own history stays on
`runs` (5.7, `fail_kind`/`fail_node`/`fail_reason`); the ledger is append-only
for `wamn_app` (SELECT + INSERT — the redrive/purge verb is a control-plane
follow-up, wamn-umt4). Rejected at the decision point: wedge+alert (leapfrog
already names the availability opt-out; a wedge here helps nobody) and a
per-flow wedge-on-terminal knob (surface for a case leapfrog mostly covers).

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
abandoned in-flight run in order).

## Guest-side queue claim (guest-self-claim, fqg.4)

The `failover` path above has the *host* claim from the queue and hand the guest a
`run_id`. The production dispatch path is the runner claiming its **own** work — and
that is the flowrunner guest's `run-next` export (fqg.4). One invocation is one turn
of the production dispatch loop:

1. **Claim + read + mark, one statement** (`claim_dispatch_sql`, fqg.18): claim
   one currently-claimable **unpartitioned** run for this replica (the exact
   `claim_batch_sql` scan — a shared fragment, so the two claim paths cannot
   drift — `FOR UPDATE SKIP LOCKED`, an owner + lease TTL), flip its run
   `dispatched → running` (the `mark_running_sql` guard, in-statement), and
   return the dispatch inputs (`flow_id`, `input_json` — the claim path drives
   the **recorded** flow + input, never a fixture constant) plus the **active
   flow version**. Empty queue → `claimed = false`, nothing to do. (Partitioned
   runs stay on the per-partition ownership path; a guest-side partitioned claim
   is fqg.9.)
2. **Plan cache** (fqg.18): the guest memoizes the parsed flow per
   `flow_id` keyed by version; the claim statement's active-version probe makes
   the cache check free, so a record stream re-fetches + re-parses +
   re-compiles **nothing** per record while a version flip (hot reload)
   invalidates on the very next record. The `app.runner` owner is likewise read
   once per instance (host-injected at instantiate, immutable after).
3. **Drive** it with the 5.2 engine, reconstructing from `node_runs` (so a
   reclaimed run resumes exactly). Each node's durable checkpoint and the lease
   heartbeat ride **one statement** (`record_success_and_renew_sql` /
   `record_error_and_renew_sql`): the claim's fresh lease covers the first node,
   each record's owner-guarded renew covers the next — the split path's
   renew-before-dispatch coverage, one round trip cheaper per node. The renew
   fires even when the record is an idempotency no-op (a cycle revisiting a
   node), so a long cyclic walk's lease stays live.
4. **Complete + dequeue atomically** (`complete_dequeue_sql` — the 5.7
   completion write and the queue removal in one statement, so no crash window
   leaves a completed run enqueued), or **park** on a `delay` (`park_sql` — push
   `available_at` to the wake and *release* the lease, so the wake re-claim is
   free; wamn-fqg.5/.7).

**Record streams (fqg.18 / D9).** Records map 1:1 to runs — there is no batch
API: a record stream is many runs of one flow, each with its own 5.7 audit
trail, per-record checkpoint, and exactly-once semantics. The amortization above
(one-statement claim/checkpoint/complete + the plan cache) is what makes that
shape cheap: the per-record cost fell from ~18 statements + a full graph
fetch/parse/compile to ~8 statements and no per-record compile (runnerbench
`stream` phase: ~66 → ~32–37 ms/record on the local debug substrate). Component
instantiation was already amortized — the run-worker instantiates the flowrunner
once and loops `run-next` for its whole lifetime. The flow-level ordering
declaration (`unordered`/`strict`/`partitioned(key)` + the dispatcher stamping
`partition_key` at enqueue) is wamn-fqg.20; the guest-side partitioned claim is
wamn-fqg.9.

**Claim-builder shape (bounded-lease correctness).** The guest is the first caller
to run `claim_batch_sql` through the `wamn:postgres` plugin (host callers use raw
`tokio_postgres`). Both claim builders therefore put the locking
`SELECT … FOR UPDATE SKIP LOCKED LIMIT n` in a CTE **fenced `AS MATERIALIZED`**
(`WITH claimed AS MATERIALIZED (…) UPDATE run_queue q … FROM claimed WHERE q.pk =
claimed.pk`). This is the load-bearing fix: **neither** a `WHERE (pk) IN (subquery)`
form **nor** a plain `FROM (subquery)` derived table is an evaluation fence — the
planner may place the `LockRows` scan on the inner side of a nested-loop join and
re-execute it once per outer row, and because `SKIP LOCKED` advances past
already-locked rows on each rescan, one statement then leases **far more than `n`**
rows (the classic Postgres `UPDATE … FOR UPDATE SKIP LOCKED LIMIT` gotcha). It
surfaced only through the plugin's cached prepared-statement execution: a `LIMIT 1`
guest claim intermittently (~plan-dependent) leased the whole batch and stranded the
extras leased until their TTL. A first rewrite to the plain `FROM`-join did **not**
fix it — which is exactly what pinned the cause to plan-driven subquery re-execution
rather than the SQL join shape. `AS MATERIALIZED` forces Postgres to evaluate the
CTE once into a tuplestore regardless of the join plan, so precisely `n` rows lock
and update. The same fence is applied to the partition sibling
(`claim_partition_head_sql`, wamn-fqg.10), which had the `IN (subquery)` form.

The lease **owner** is *host-injected*: the `wamn:postgres` plugin sets an
`app.runner` GUC (from the workload's `wamn.runner` config, per replica) alongside
the tenant claim, and the guest reads `current_setting('app.runner', true)` — a
non-spoofable per-replica identity to lease/renew under. The guest links
`wamn-run-queue` with `default-features = false`, so only the pure claim-path
builders enter its wasm (the cron/dispatch pair — croner/chrono — stays
host-side behind the default `dispatcher` feature).

The `failoverbench` `claim`/`park`/`heartbeat` gates *seed* `run_queue` directly
(the write-ahead + enqueue a dispatcher would do) and drive `run-next` — proving
the guest claim path end-to-end against a real Postgres. What consumes the queue as
a **running service** is the production runner below.

## Production runner (`run-worker`, fqg.8)

The `wamn-run-worker` binary (its own SR9 artifact) is the always-on service
that **closes the live dispatcher → `run_queue` → runner chain**: a long-lived
process that instantiates the flowrunner component once (baked into its image at
`/components/flowrunner.wasm`) and loops the guest's `run-next` export, so the runs
the dispatcher write-ahead + enqueued are actually claimed and driven to
completion. The loop core is `wamn_run_worker::RunWorker` — `instantiate`
(inject the host-side `app.runner` owner + tenant + `search_path`), `drain` (pull
every currently-claimable run — each `run-next` claims one and drives it terminal
or parks it, so the claimable set strictly shrinks and the drain terminates), and
`serve` (drain → wait → repeat).

Drain termination relies on each `run-next` itself terminating. A permitted graph
**cycle** (loops are a flow feature; only self-loops are rejected at validation)
could otherwise drive forever inside one `run-next` — renewing its lease per node
so the janitor/failover never reclaim it, wedging the runner (cjv.4 / review
C2-1). The bound is the engine's per-invocation **dispatch budget**
(`wamn_runner::Plan::set_dispatch_budget`, default 10 000 node executions,
retries included): once spent, the run fails with the terminal
`runaway-budget` fail kind and **dequeues** through the ordinary
`outcome = 2` path, freeing the runner. So every claimed run terminates, parks,
or exhausts its budget — the drain always ends. Reconstruction on a resumed run
is exempt (recorded history never counts against the live budget). The
remaining gap — a single node that never returns (infinite compute inside one
node) — is the run-worker's `set_epoch_deadline(u64::MAX / 2)` backstop
deliberately deferred to wamn-dq5 [5.12], whose epoch-cancel machinery owns
trap handling + re-instantiation.

**Single-project** (one Deployment per project — the api-gateway analog,
`deploy/platform/runner.yaml`): one flowrunner instance whose plugin session carries this
project's identity. The lease **owner** is per-replica (the pod name via
`WAMN_RUNNER` from the downward API), so `FOR UPDATE SKIP LOCKED` + attributable
leases make replicas and scale-out safe, and a dead replica's lease ages out for
another to reclaim + resume (fqg.2). A multi-project runner (a dispatcher-style
projects file, N instances) is a follow-up.

**Idle handling** mirrors the dispatcher (NATS-optional): a doorbell hint on
`wamn.doorbell.<tenant>` — the subject the dispatcher already publishes to — wakes
an immediate drain, and a poll-with-backoff reconcile (the dispatcher's
`next_interval` cadence) guarantees pickup even when a hint is lost or NATS is
absent. **SIGTERM** is handled explicitly (PID 1 gets no default disposition) so a
rollout exits in milliseconds; abrupt death is safe anyway — the lease ages out and
another replica reconstructs from `node_runs`, exactly-once via the sink
idempotency. A drain error is non-fatal (logged + backed off): the pool re-dials on
the next call.

**Credentials + egress (5.9)** — the runner carries this project's credential
vault (`--credentials-file` / `WAMN_CREDENTIALS_FILE`, a `{project: {name:
secret}}` JSON mounted from a K8s Secret; `--project` picks the key) and the
outbound-`wasi:http` egress handler its flows' http-request nodes need: the
fork's `check_allowed_hosts` over `DefaultOutgoingHandler`, gated by
`--allowed-hosts` / `WAMN_ALLOWED_HOSTS` — **EMPTY = DENY-ALL, fail-closed**
(an unlisted host fails `egress-denied`; per-flow allowlists are the fqg.11
refinement). Without a handler an outbound call would trap and poison the
instance. See [credential-vault.md](credential-vault.md).

The `runnerbench` gate drives the *production* `RunWorker` (not a gate-local
worker) against an ephemeral schema seeded the dispatcher way, asserting it drains
the queue to completion, reuses one instance across drains, and reports an empty
drain — the local, repeatable, mutation-tested counterpart of the in-cluster
dispatcher → queue → runner live smoke. Its `stream` phase (fqg.18) pushes
`--stream-records` (default 200) record-runs of one flow through one warm
instance — correctness (every record completes exactly once with a full
per-record `node_runs` trail and the v1 sink witness) plus the wall-clock
per-record measurement the amortization is judged by — and its `stream-reload`
phase activates a new flow version mid-stream and asserts the following records
run it: the load-bearing guard on the guest plan cache's invalidation.

## Trigger dispatcher (cron + parked-wake)

> **Outbox path RETIRED (D19 v3 §3 teardown, executed 2026-07-20,
> wamn-l5i9.19).** Row-event capture is CDC via logical decoding
> (`docs/event-plane-jetstream.md`): the reader publishes onto JetStream and
> the **materializer** fires registered flows (`{flow}:evt:{stream_seq}` run
> ids) — the outbox poller, the per-table triggers + DDL emission, the outbox
> table + GC, and the l5i9.18 cutover scaffolding (`evt_shadow`, registration
> `state: shadow`, the dispatcher's yield guard) are deleted. Cron and
> parked-wake were unaffected and remain here.

The **shared trigger dispatcher** is the always-on control-plane loop that turns
*time* into runs: it owns **cron schedules** (flows with a
`cron` trigger, F3) across **all projects**, and **wakes parked runners**
whose `available_at` has arrived — one service, per-project connections (D3:
reconciliation follows connection ownership, no cross-DB sweep), adaptive
intervals, no per-project listeners, no polling herd. It is the second driver of
this crate: the decisions — cron due-tick evaluation, run-id
minting, the poll cadence — live in the pure `cron`/`dispatch` modules
and take an injected `now`, so the `dispatchbench` gate fast-forwards a nightly
cron and a three-day outage in milliseconds (the 11.1 fast-forwardable-cron
discipline); `wamn-dispatcher` supplies the real clock.

One sweep of one project:

1. **Registry.** Scan active flows: the trigger lives *inside* `graph_json`
   (wamn-flow `Flow.trigger`) — `cron` registers here; `webhook`
   is the gateway's, `manual` the editor's, and `row-event` the
   **materializer's** (its event registration is the trigger record —
   l5i9.16/.17). A flow that fails to parse or
   validate is skipped with a warning, never wedging the project's dispatch.
   A row whose `flows.flow_id` **column** differs from the graph's validated
   `flow-id` is treated the same way (skipped): run ids
   are minted from the column, so the equality requirement extends the 5.1
   flow-id slug charset — which `validate()` enforces only on the graph field
   — to the id that is actually embedded in `{flow}:cron:{tick}`.
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
3. **Wake / reconciliation.** One read-only scan (`parked_due_sql`) surfaces
   every currently-due, unleased, budget-remaining queue row — a parked `delay`
   wake, or a run whose enqueue-time hint was lost — and hints each on the
   doorbell. Duplicate hints are harmless by design (the claim is the arbiter),
   which is what lets one scan double as the lost-hint reconciliation backstop
   of lifecycle step 5.
4. **Cadence.** Per-project adaptive interval (`next_interval`): work tightens
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
run per (flow, cron tick) — and both write-ahead and
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

Deployment shape: `wamn-dispatcher --projects-file <json>` (one entry per
project: `url` + `tenant` + `schema` — the 2.2 projects-file pattern) or the
single-project flags. The production manifest is `deploy/platform/dispatcher.yaml`: a
2-replica Deployment (no leader — replicas race and collapse on the write-ahead
`ON CONFLICT`, the dispatchbench `race` gate; scale-out is safe by the same
argument) + a PodDisruptionBudget, with the projects file mounted from the
`wamn-dispatch-projects` Secret and the doorbell's mTLS NATS material from
`wasmcloud-runtime-tls` (the queuebench-job pattern; a publish-only NATS
identity is a tracked follow-up — the runtime cert maps to an
allow-all-subjects user). The Secret is deliberately a SEPARATE manifest
(`deploy/platform/dispatcher-projects.example.yaml`, demo values pointing at the
`wamn_dispatch_demo` schema — production run-state.sql + run-queue.sql objects
plus the `flows` registry, whose production DDL is now `deploy/sql/flows.sql`
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
(deploy/sql/run-state.sql) — an index-only backward scan for `cron_last_run_sql`'s
per-flow `max(run_id)` instead of a seq scan. Proven live in-cluster
(2026-07-12): two replicas against the demo project minted 4 consecutive 20s
cron ticks exactly once each (the then-live outbox row fired + acked too —
that path is retired), `EXPLAIN` confirmed the anchor query uses
`runs_cron_anchor`, and SIGTERM shutdown measured 13 ms.

### Scale-to-zero wake (POC-F3, wamn-fqg.12)

A runner Deployment can be **parked at 0 replicas** to save idle cost; a cron
fire (or any enqueue) must then be able to **wake** it. The three pieces already
in this crate's doorbell story compose into the wake path with one small new
actuator:

- **doorbell = the wake signal.** The dispatcher's cron fire enqueues the run
  and publishes `wamn.doorbell.<tenant>` exactly as above — but a hint published
  while the runner is at 0 replicas has no subscriber and is *lost*.
- **dispatcher re-hint = the retry.** The wake / reconciliation scan
  (`parked_due_sql`, lifecycle step 3) re-hints every currently-due unleased
  queue row on **every** sweep, and duplicate hints are harmless by design — so
  a lost first hint self-heals on the next sweep. No first-hint delivery
  guarantee is needed.
- **waker = the actuation.** A tiny always-on service (`wamn-waker`,
  `deploy/platform/waker.yaml`) subscribes to the doorbell for its configured
  tenants and, on a hint whose mapped runner Deployment sits at 0 replicas,
  scales it `0 -> 1` via the Kubernetes `apps/v1` Deployment `scale`
  subresource. The woken runner then subscribes to the same doorbell and drains
  the enqueued run. The waker's decision is pure over
  `(tenant, mapping, current_replicas)` — a no-op unless the tenant is mapped
  AND the Deployment is at exactly 0 — and it has **no polling loop of its own**
  (the dispatcher re-hint is its only retry). It scales **up only**: idle `-> 0`
  scale-down automation is out of scope (a follow-up).

The waker is the **one** wamn component granted a Kubernetes privilege
(`deployments/scale` get+patch, namespace-scoped, `automountServiceAccountToken:
true`): the dispatcher deliberately never talks to the API server
(`deploy/platform/dispatcher.yaml`, `automountServiceAccountToken: false`), and a
dedicated actuator keeps that boundary intact. **KEDA** (a `ScaledObject` on a
queue-depth trigger) is the obvious off-the-shelf alternative and was **not
taken**: it is an owner-level third-party infra install, whereas a ~200-line
service that reuses the existing doorbell needs nothing new in the cluster.
Proven end-to-end by the `wakeproof` gate.

## Scope (5.14) vs. siblings

This issue built the D3 hybrid queue: the SKIP LOCKED queue, the write-ahead / fast
path, single-owner leases + reclaim, the janitor, the reconciliation cadence,
**per-partition ownership** for `partitioned(key)`, **checkpoint/resume on
replica loss**, the **shared trigger dispatcher** (cron + parked-wake, above;
the outbox half was retired at l5i9.19), the **guest-side queue claim** (`run-next`, above), and the **production
runner** (`run-worker` + `deploy/platform/runner.yaml`, fqg.8 — closes the live
dispatcher → queue → runner chain), proven by `queuebench` + `failoverbench` +
`dispatchbench` + `runnerbench`. It deliberately does **not** ship (tracked as
follow-ups):

| Deferred | Where |
|---|---|
| A **multi-project runner** (`run-worker` is single-project — one Deployment per project; a dispatcher-style projects file + N flowrunner instances) | 5.14 follow-up |
| **Guest-side partitioned claim** (`run-next` claims only unpartitioned runs today; partitioned runs stay on the per-partition ownership path) | 5.14 follow-up |

And it does not own: the engine walk / retry / reconstruction (5.2 + 5.7 — the
claimed run drives them); the `runs`/`node_runs` schema (5.7 — 5.14 co-transacts
and reuses the reserved statuses); per-node ordering *semantics* (5.11 — 5.14
provides the per-partition claim *mechanism*); the cancel operation (5.12); the
payload byte store (5.10). The guest's **direct** exports (`run`/`run-s6`) are
unchanged; `run-next` is the additive claim path.

## Gates

- **`cargo test -p wamn-run-queue`** — the pure decisions + SQL shape: claim
  eligibility (Ready/Leased/Parked/Exhausted — incl. the wamn-fqg.7 rule that a
  budget-spent row wakes iff its lease was released, not merely expired),
  `plan_claim` ordering + limit (and
  that it skips partitioned rows), lease liveness + renewal, janitor orphan-detection,
  reconciliation cadence, **per-partition ownership** (`plan_acquire`,
  `plan_partition_claim` head-first + one-in-flight), the **dispatcher decisions**
  (cron `next_fire`/`due_tick` incl. leap-day/short-month calendar edges,
  sub-second tick canonicalization, misfire collapse; deterministic
  run-id minting + ordering; the adaptive interval), the
  SQL builders' `SKIP LOCKED`/tenant-scoping/`RunStatus` literals/`::text::jsonb`
  binding, record JSON round-trip, and the `deploy/sql/run-queue.sql` drift guards
  (queue + partition) — all off-cluster.
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
  and in-order advance once the head dequeues; plus the dispatcher's cron
  last-tick recovery (flow-exclusive `max(run_id)` — cross-flow poison ids,
  including a nested `{flow}:cron:` prefix in a *neighboring* flow id, never
  leak into the anchor) and the wake scan
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
  in-cluster. The separate **ceiling** mode (wamn-z7b.1, EVT-C7 —
  `docs/event-plane-jetstream.md` §10) is a measurement *campaign*, deliberately
  **not** part of `--mode all`: open-loop producers + closed-loop claimers drive
  the full lifecycle (write-ahead+enqueue → claim → complete) through a find-knee
  ramp on the production combined-statement path and the split-builder path at
  batch 1/8/32, a sustained soak at 80% of knee with a bloat probe, and a 10×
  burst/recovery profile — curves + CSVs published to `docs/ceilings.md`, with
  only the exactly-once/completeness sanity asserts acting as pass/fail
  (`deploy/gates/queuebench-ceiling-job.yaml`).
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
  the resume's unconditional completion write still wins). The `claim`/`park`/
  `heartbeat` modes (fqg.4) drive the guest's own `run-next` claim path against
  the same schema: `claim` proves a single runner **drains** the queue
  (claim→drive→dequeue) and **N concurrent replicas drain it exactly-once**
  (`SKIP LOCKED`, max one sink row per run), plus the **wrong-flow** guard (a run
  recorded as `alt-flow` drives `alt-flow`'s reverse → sink `"tpiecer"`, not a
  hard-coded `poc-receipt` → `"RECEIPT"`); `park` proves a `delay` run **parks
  and releases its lease** then a later `run-next` re-claims and completes; and
  `heartbeat` proves the per-node lease **renewal advances** `lease_expires_at`
  across a long walk (deterministic — a lease-value poll, no steal race). All the
  race/guard/claim assertions are mutation-tested. Same co-located, no-CPU-limit
  topology as `queuebench`, locally and in-cluster.
- **`dispatchbench`** — the trigger dispatcher's gate of record (pure host-side,
  no wasm guest), driving the *real* `Dispatcher` engine with **stepped time**
  against two superuser-provisioned ephemeral project schemas. `cron`: a nightly
  F3-shaped schedule fires exactly once per due tick — not early, once within a
  tick's second, no duplicate across a dispatcher **restart** (anchor recovered
  from the run ids), **misfire collapse** after a simulated three-day outage,
  first-sight bootstrap, and the fire's write-ahead + enqueue co-transaction
  proven atomic by an enqueue-side trap. `ordering`: the flow-level ordering
  declaration (5.11, wamn-fqg.20) is stamped onto `run_queue.partition_key` at
  fire() over the cron envelope — unordered→NULL, strict→the constant flow key,
  partitioned→the evaluated JMESPath key, with a missing key falling back to
  the flow-wide stream (never NULL) — and the flow's D20 `partition_policy` is
  materialized coherently (wamn-kq0z). `race`:
  **two live dispatchers ticking concurrently** over one project — every cron
  tick still fires exactly once (won-insert count == distinct
  runs) with the contention itself proven (losing attempts counted), no
  leader. `fairness`: a 120-run due parked backlog in project
  A is wake-hinted batch-bounded per sweep **oldest-first** and does not starve
  project B's
  first sweep; the adaptive intervals tighten/decay per project independently.
  `wake`: a parked run is doorbell-hinted only once due; a cron firing's hint
  carries the won run id and arrives only after its transaction committed.
  `live`: the real `dispatch` run loop keeps an every-second cron firing
  **beside a permanently failing project** (isolation), survives its DB
  connections being killed (reconnect), and honors the cron-aware sleep (a
  tick under a fixed 5 s interval fires within ~1 s). Every load-bearing check
  is mutation-tested (an observed-now tick identity, a policy-blind enqueue,
  and a cron-blind sleep each fail the gate). Same co-located, no-CPU-limit
  topology as `queuebench`, locally and
  in-cluster.
- **`flowbench` (S3) + `testhostbench` (S6)** — regression: the flowrunner guest is
  unchanged, so both stay green.

# Run queue — global FIFO claim and reclaim

> **Read through the deployment-simplification ruling (owner-ratified
> 2026-08-16, `wamn-0h0g.13.43`,
> `docs/deployment-simplification-spec.md`).** This document's
> resolution-map and release-pin references are superseded: claim step
> 4 (resolve and verify the complete immutable `run_flow_resolutions`
> map, with a retry reproducing an identical map), step 5's "only
> after the map succeeds" ordering, the "materialize/verify the map"
> leg of the **ordinary** classification, both the "catalog" and the
> "resolution-map" facts in the **expired pre-effect** preservation
> list, the typed resolution refusal paragraph, and the
> `map identity/refusal` and `map-before-lease` proof-floor lines are
> all superseded. Most die with `run_flow_resolutions` ("Deleted by
> this ruling"); the preservation list's "catalog" item is the
> admitted run's catalog pin and dies instead with `.2.4`'s
> admission-time bundle pin, which moves to claim-time recording
> (`docs/deployment-simplification-spec.md:90-91`). The claim becomes
> **lock → classify → lease** plus one write-once record of
> `(release version, manifest digest)` taken from the claiming pod's
> identity; resolution is a pure read of that pod's mounted release
> manifest, and that record is what a reclaim preserves in place of
> the two struck items. The column shape is `wamn-0h0g.15.11`'s work,
> not this marker's. Everything else here stands unchanged: the
> durable record, the global FIFO order and its index prefix,
> visibility and lease rules, the effect-attempt advisory fence, the
> three classifications and their preservation semantics, `attempts`
> accounting, the exhausted pre-effect janitor, doorbells and dispatcher
> reconciliation, and the retired partition plane. The code, SQL, and
> guard deletions land in the wave's claim-path commit
> (`wamn-0h0g.15.10`, `.15.11`).

The run queue is the durable handoff between admission and the warm run worker.
PostgreSQL owns queue truth; NATS doorbells are latency hints only. The one
production claimant lives in the trusted host and gives the flowrunner guest
only an already-claimed `(run-id, payload)` pair.

The schema of record is [`deploy/sql/run-queue.sql`](../../../deploy/sql/run-queue.sql).
Pure queue decisions and SQL builders live in
`crates/execution/run-state/src/queue`; the transaction composer lives in the
trusted `wamn:postgres` runtime plugin.

## Durable record

`wamn_run.run_queue` contains one pending or in-flight row per run:

| Column | Meaning |
| --- | --- |
| `tenant_id`, `run_id` | Tenant-scoped identity and FK to `runs`. |
| `available_at` | Visibility time; a future value parks the run. |
| `stream_seq` | Numeric CDC ordering tiebreak; `0` for non-event admission. |
| `lease_owner`, `lease_expires_at`, `lease_generation` | Current execution authority. |
| `attempts`, `max_attempts` | Crash-evidence count and pre-effect reclaim budget. |
| `priority` | Reserved; it does not participate in claim order. |
| `enqueued_at` | Immutable queue admission time. |

Every row belongs to one tenant-global FIFO. The exact claim order and index
prefix are:

```text
(available_at, stream_seq, run_id)
```

`available_at <= now()` gates visibility. A live lease hides a row; an expired
lease makes it reclaimable. `FOR UPDATE OF runs, run_queue SKIP LOCKED LIMIT 1`
lets concurrent workers take disjoint rows without blocking each other. The
locking candidate CTE is `AS MATERIALIZED`, preventing a prepared-statement
plan from rescanning past the one-row limit.

## One production claim transaction

The host performs the following in one tenant-bound transaction:

1. Lock one eligible queue row in the exact FIFO order, without granting a
   lease or reading a plan.
2. Acquire the tenant/run effect-intent advisory fence.
3. Read immutable effect-attempt evidence from a fresh `READ COMMITTED`
   statement and classify the row.
4. For an executable classification, resolve and verify the complete immutable
   `run_flow_resolutions` map. A retry must reproduce the identical complete
   map; incomplete or mixed maps refuse.
5. Only after the map succeeds, grant a fresh lease/generation, transition
   `dispatched` to `running`, and return authoritative input.

The classifications are:

- **ordinary** — no prior lease. Preserve state, materialize/verify the map,
  then grant the first lease.
- **expired pre-effect** — a prior lease exists and no immutable effect attempt
  exists. Delete replaceable `node_runs`, clear `runs.state_json`, preserve all
  admission, input, context, catalog, event, deadline, idempotency, immutable
  evidence, and resolution-map facts, then restart from zero.
- **expired with effect intent** — any immutable attempt wins regardless of
  crash budget. Store the exact `effect-uncertain` caller outcome when still
  releasable, preserve any prior caller winner, and delete the queue row. No
  lease or guest entry follows.

A typed resolution refusal similarly stores the exact generic failed caller
envelope when applicable, marks the run failed, and dequeues it before any
lease grant.

`attempts` counts crash evidence only: granting over a prior non-NULL lease
increments it; first claim and park/wake reclaim do not. An expired pre-effect
row below budget can restart. An expired row at budget is handled by the
production janitor instead of the claimant.

## Effect-attempt serialization

The private effect writer and both claim/reaper classifiers use the same
transaction-scoped advisory key derived from `(tenant_id, run_id)`.

- The writer takes the fence, then uses a fresh statement-time runnable-state
  check before inserting a new occurrence.
- An exact occurrence retry remains readable after terminalization because the
  immutable ledger outlives the run/queue rows.
- A divergent retry still refuses, and a new occurrence without a running run
  and live lease returns `EffectWriterErrorKind::RunNotRunnable`.
- The claimant and janitor lock the queue/run candidate, take the fence, then
  read effect evidence from a fresh snapshot.

This closes the no-FK absence race without coupling the independently retained
effect ledger to `runs`. The private writer has table DML only on its ledgers;
its non-ledger authority is column-level `SELECT` on exactly
`runs.(tenant_id,run_id,status)` and
`run_queue.(tenant_id,run_id,lease_owner,lease_expires_at)`.

## Exhausted pre-effect janitor

The host runs one janitor turn when the claim loop finds no work. It selects an
expired, budget-exhausted row in the same FIFO order and under the same
run/queue locks. After taking the effect fence:

- an immutable attempt leaves the row for the claimant's `effect-uncertain`
  classification;
- an effect-free row becomes `infrastructure-failure`, receives the exact
  generic caller outcome/hash when still releasable, and is dequeued;
- a callerless run keeps caller fields NULL, and an already released caller
  keeps its exact winner.

## Doorbells and reconciliation

Admission and the event materializer insert unleased rows. A NATS-core doorbell
wakes a zero-scale worker quickly, but duplicates and loss are harmless because
PostgreSQL arbitration is authoritative. Dispatcher reconciliation reads due
rows and queue depth to backstop missed hints; it does not claim or mutate
queue rows.

## Retired partition plane

Authored ordering, partition keys/policies, partition-owner leases, partition
claim APIs, and `run_dead_letters` are absent. Existing schemas converge through
the leading `PartitionPlaneCutover` in `reconcile-run-plane`:

- observed legacy tables are locked `ACCESS EXCLUSIVE`;
- active or unobservable leases refuse before DDL;
- any dead-letter row refuses with
  `retired-run-dead-letter-history-requires-archive-or-environment-reprovision`;
- stored flow JSON containing retired ordering fields refuses with
  `retired-authored-ordering-requires-environment-reprovision`;
- only a safe empty/drained shape loses the retired columns/tables/indexes and
  receives the global FIFO index.

No compatibility reader or data-rewrite path exists.

## Proof floor

- `wamn-run-state` unit tests pin eligibility, ordering, classification, reset,
  terminalization, janitor, and SQL shapes.
- `production_claim_live` runs the real PostgreSQL 18 transaction path and
  proves FIFO concurrency, double-claimer exclusion, map identity/refusal,
  candidate override, pre-effect preservation, exact caller replay, and the
  private-writer/claim/reaper fence race.
- `run_plane_live` proves locked refusal/rollback, partial legacy shapes,
  empty convergence, exact writer ACL repair, and idempotence.
- `tools/gate-mutants/global-fifo-claim.sh` mutation-pins the load-bearing
  ordering, classification, fence, map-before-lease, outcome, janitor, and
  cutover guards.

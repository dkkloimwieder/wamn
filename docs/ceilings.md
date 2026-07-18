# Measured ceilings

The capacity-model ledger the event-plane program publishes into
(`docs/event-plane-jetstream.md` §10 "curves and ceilings, not verdicts" +
§11 numbers hygiene). Every figure carries its measurement date, environment,
git rev, config knobs, and a raw-data pointer. **Curves over single numbers,
always** — the per-level CSVs in `docs/ceilings-data/` are the record; the
prose below is a reading of them. No pass/fail rules are attached here; the
owner attaches decision rules after the numbers exist.

## C7 — run-queue full-lifecycle transitions/sec (wamn-z7b.1)

**Date:** 2026-07-18 · **Git rev:** the `wamn-z7b.1` commit (parent `817937d`;
the campaign binary was built from exactly that tree) · **Bench:**
`wamn-gates queuebench --mode ceiling` (`deploy/queuebench-ceiling-job.yaml`),
run twice back-to-back for repeatability.

**What "one transition" is:** the full production lifecycle — a write-ahead
`runs` row + `run_queue` row committed in one producer transaction, then
claimed and completed by a claimer. Sojourn = enqueue-transaction start →
completion. Two claimer shapes are measured: the **combined** production
run-worker path (fqg.18: `claim_dispatch` + `complete_dequeue`, two statements
per run) and the pre-fqg.18 **split** path (`claim_batch(b)` + per-run
dispatch read + mark-running + complete + dequeue, 4 statements per run after
the claim) at batch 1/8/32.

**Environment (read the caveat):** the p0 reference class — a 3-node kind
cluster on a single developer workstation, the shared `deploy/postgres.yaml`
pod (postgres:18, **stock container config — default `shared_buffers`,
autovacuum, fillfactor; no tuning** — except `fsync=off` +
`synchronous_commit=off`, which the fixture pod has always run with
[deploy/postgres.yaml], so every latency in this file excludes per-commit
fsync; correction recorded 2026-07-18 during C2), the gates Job co-located
with the Postgres pod, no CPU limit (the S2 CFS lesson), release build. 8 open-loop
producer connections (catch-up pacing), 12 closed-loop claimers, statements
prepared once per connection (the plugin's `prepare_cached` wire shape),
60 s per ramp level, base rate 250/s, bisect tolerance 15%, lease TTL 60 s.
**Caveat — this rig is noisy:** all three kind nodes and the host's other
containers share one physical disk, and roaming multi-second stall windows
(fsync/checkpoint pressure, host tenants) poisoned *different* ramps in each
run (examples: run 1 combined @750/s p99 14.4 s while the same path completed
999/s cleanly one level earlier; run 2 split-b1 @500/s p99 17.7 ms against a
3.6 ms baseline, with the *subsequent lower* levels back at 3.5 ms). Noise is
strictly one-sided (it can only push a level toward "saturated"), so the
reported knee per path is the **max across the two runs**; single-run knees
on this rig carried up to ~3× downward noise. A production-grade re-measure
belongs on quieter hardware (phase 2, `wamn-z7b.6`).

### Knees (60 s levels; saturation = p99 doubling vs baseline, completions
### diverging >10% from achieved enqueues, or drain timeout)

| Path | Run 1 knee | Run 2 knee | **Reported (max)** | p99 at knee |
|---|---|---|---|---|
| combined (production) | 688/s ⚠noise | **2000/s** | **~2000/s sustained** | 4.3 ms |
| split batch=1 | 2000/s | 438/s ⚠noise | ~2000/s | 5.3 ms |
| split batch=8 | **2500/s** | 562/s ⚠noise | ~2500/s | 6.1 ms |
| split batch=32 | 625/s ⚠noise | **2000/s** | ~2000/s | 5.8 ms |

Readings from the curves (`run{1,2}-ramp-*.csv`):

- **The clean combined curve (run 2)** is flat — p99 3.5–4.3 ms from 250/s
  all the way to 2000/s — then the tail blows out while throughput keeps
  going: 4000/s offered still *completed* 3998/s at p99 221 ms, and
  2250–3000/s offered settle at ~2100–2160/s completions with multi-second
  tails. So on this rig the **latency knee is ~2000/s** and the **overload
  completion plateau is ~2100–2200/s** for the combined path.
- **Batch-claim size barely matters below the knee.** All four paths converge
  on ~2000–2500/s: at sub-saturation the queue is nearly empty, so a
  `LIMIT 32` claim returns 1–3 rows anyway. Where batch shows is **overload
  drain**: split-b8 completed 3997/s at 4000/s offered (p99 109 ms) and
  split-b32 completed 3998/s at p99 20.5 ms — the amortized claim scan drains
  a deep backlog measurably faster than the one-row combined statement
  (~2150/s plateau). Batch 32 buys drain throughput at the cost of in-batch
  head-of-line sojourn once backlog exists.
- The combined statement's two-round-trip lifecycle and the split path's
  five-statement lifecycle land within noise of each other at the knee on a
  co-located pod — the fqg.18 win is round-trip *count* (guest↔host↔DB), which
  this co-located rig prices at near zero; it matters at real network RTTs.

### Sustained soak (300 s at 80% of knee, combined path, bloat probed)

Two very different soaks (`run{1,2}-soak-*.csv`):

- **Run 1 @ 550/s** (80% of its noise-depressed knee): dead flat — 549.7/s in
  every 30 s window, p99 3.53–3.63 ms throughout. `run_queue` grew 8 KiB →
  1.97 MiB and plateaued; dead tuples sawtoothed ~11k↔44k as autovacuum
  cycled; queue depth ≈ 0. **550/s is comfortably sustainable untuned.**
- **Run 2 @ 1599/s**: a **bloat-degradation boom-bust**. The first 60 s keep
  up (1598/s, p99 4→15 ms) while dead tuples outrun autovacuum (95k by
  t=30 s); the claim scan degrades and throughput collapses (562/s, p99
  18.7 s at t=90); backlog builds to ~50–69k rows; autovacuum catches up and
  the claimers surge to ~2160/s and drain it; repeat. Mean over the soak:
  **~1365/s at 1599/s offered**, relation plateaued at 17.5 MiB, dead-tuple
  peaks ~185k. **The 60 s-window knee does not hold as a sustained rate with
  stock autovacuum** — the sustained smooth ceiling lies between 550/s
  (proven flat) and ~1600/s (oscillates). Tightening it is exactly the
  phase-2 fillfactor × autovacuum matrix (`wamn-z7b.6`).

### Burst (10× the soak baseline for 60 s, recovery measured)

- Run 1 (base 550/s, spike 5.5k/s offered): peak backlog 55,676 rows; depth
  back under threshold **26 s** after burst end; fully drained 30 s after.
- Run 2 (base 1599/s, spike 16k/s offered): peak backlog 103,857 rows; spike
  p99 50 s (pure queueing — by design at 10× overload); depth recovered
  **66 s** after burst end. The queue itself never corrupted: exactly-once +
  completeness held at every level of both runs (the riding sanity asserts).

### What this retires (D3)

D3's folklore said *"a tuned single-table `SKIP LOCKED` queue sustains ~1–5k
run-state transitions/sec"*. Measured, untuned, on the reference rig: 60 s
knee **~2000–2500/s**, overload drain **~4000/s**, sustained smooth ceiling
**~550–1400/s** with stock autovacuum. The folklore's range brackets reality
and its "tuned" qualifier is demonstrably load-bearing — the D3 revisit
trigger (">~1k discrete runs/sec sustained in one project") sits right at the
untuned sustained boundary, so a project approaching it needs the phase-2
tuning pass before any architectural conclusion is drawn.

### Raw data

- Aggregated per-level curves: `docs/ceilings-data/run{1,2}-ramp-{combined,split-b1,split-b8,split-b32}.csv`,
  `run{1,2}-soak-{windows,bloat}.csv`, `run{1,2}-burst-depth.csv` (extracted
  from the job logs' `=== BEGIN CSV … ===` blocks).
- Full job logs: session scratchpad `ceiling-run{1,2}.log` (ephemeral; the
  CSVs + this reading are the durable record).
- Reproduce: `docs/build-and-test.md` § [EVT-C7 / wamn-z7b.1].

### Deferred (phase 2 — `wamn-z7b.6`)

Fillfactor × autovacuum matrix, the 30-min soak, the 1M-run bloat soak, a
quieter host, and a noise-robust ramp (retry a saturated level once before
accepting it — `wamn-z7b.7`) so single runs stop carrying 3× downward noise.

## C2 — outbox trigger overhead (wamn-z7b.2)

**Date:** 2026-07-18 · **Git rev:** the `wamn-z7b.2` commit (parent `b24092a`;
the campaign binary was built from that tree — post-run edits were
comment/doc-only) · **Bench:** `wamn-gates outboxbench --mode all`
(`deploy/outboxbench-job.yaml`).

**What is measured:** the cost the *customer* pays for D4 row events — the
wp4-emitted `AFTER … FOR EACH ROW` trigger (`Migration::outbox_triggers`)
writing one `outbox` row per entity-table write, quantifying R8c's
"write amplification" adjectives. Paired same-table A/B: the bench applies the
REAL emitter plan, measures, applies the REAL drop plan, re-measures — a
closing baseline re-run bounds heap/cache drift (it agreed with the opening
baseline within ~5 µs / ~16 B per row, so the with/without delta is the
trigger). Knobs: 1000 ops per single-row batch, prepared statements,
VACUUM + CHECKPOINT before every measured batch (each batch pays the same
full-page-image regime — FPIs on outbox pages are a real production cost of
the trigger, so they belong in the delta). WAL measured as
`pg_current_wal_insert_lsn()` deltas — WAL *generated*, unaffected by the
fixture pod's `fsync=off`/`synchronous_commit=off` (the flushed-position
variant reads ~0 under async commit; an instrument bug caught and fixed
mid-campaign).

**Environment:** the p0 reference class (the same rig, pod, and co-location
as C7 above — including its noise caveat), with one addition to the record:
`deploy/postgres.yaml` runs `fsync=off` + `synchronous_commit=off`, so every
latency here (and in C7) excludes per-commit fsync. WAL byte counts are
deterministic and unaffected. **Single-run record** — a deliberate deviation
from C7's two-run practice: C2 has no knee search a one-sided stall can
poison; its headline numbers are byte counts and medians. The broken-WAL
first run's valid columns corroborate: latencies agree within a few µs,
growth peaks/finals within 5%.

### Single-row overhead (`c2-trigger.csv`)

| op | baseline p50 / WAL·row | with trigger p50 / WAL·row | trigger delta |
|---|---|---|---|
| INSERT | 54 µs / 253 B | 83 µs / 739 B | +29 µs (×1.5), +486 B (×2.9) |
| UPDATE | 65 µs / 366 B | 94 µs / 879 B | +29 µs (×1.4), +513 B (×2.4) |
| DELETE | 46 µs / 215 B | 72 µs / 733 B | +26 µs (×1.6), +518 B (×3.4) |

A registered table pays **~+30 µs and ~+500 B of WAL per single-row write**
(the outbox row: the full `to_jsonb` after-image payload + the pending-index
entry). p99s stay tight (0.15–0.19 ms with the trigger) — no tail regime
change, just a constant per-write tax. Exactly-once held: 1001/1001/1001
outbox rows per event for 1001 writes each (the sanity gate).

### Bulk single-statement UPDATE (`c2-bulk.csv`) — the R8c number

| rows | duration off → on | WAL/row off → on | amplification |
|---|---|---|---|
| 1k | 4.5 → 22.7 ms | 421 → 890 B | duration ×5.1, WAL ×2.1 |
| 10k | 41.9 → 232.8 ms | 385 → 841 B | duration ×5.6, WAL ×2.2 |
| 100k | 406 → 2322 ms | 384 → 836 B | duration ×5.7, WAL ×2.2 |

A bulk write on a registered table pays a **×5–6 transaction-duration
amplification and ×2.2 WAL** — 100k rows go from 0.4 s to 2.3 s inside the
user's transaction, plus 100k outbox rows. Duration amplification far
exceeds WAL amplification: the per-row plpgsql invocation + outbox INSERT
execution dominates, not the bytes. This is the number `wamn-vbl`
(registration-driven per-entity emission) is sized against — an
*unregistered* table pays none of it, and today's uniform all-tables plan
charges every table. R8c's gate-before-bulk-import-tooling stands.

### Growth vs GC cadence (`c2-growth-c{0,60,600}.csv`)

200 events/s sustained row-event INSERT load (trigger-fired), a 1 s
dispatcher-shaped acker, prune retention shortened to 5 s so the **cadence**
is the swept variable (production retention is 7 days — steady-state acked
history is `rate × retention` on top of these curves and dominates them; the
d8v `--outbox-retention-hours` knob prices that separately):

- **cadence off:** unbounded by construction — 60k rows / 17.9 MiB after
  300 s, linear. What every project looked like before wamn-d8v.
- **cadence 60 s:** bounded — relation peak 8.5 MiB, final backlog ~1.1k rows.
- **cadence 600 s (the shipped d8v maintenance interval):** a clean bounded
  sawtooth over two full cycles — the outbox climbs to ~109k rows / ~37 MiB
  per cycle, each prune tick drains the acked backlog (batch-bounded, ~24 ×
  5k batches) back to ~1.2k rows in one tick, and the relation high-water
  **plateaus** at 36.9 MiB (pages recycled, not returned). Operational sizing
  rule: outbox high-water ≈ `event-rate × prune-cadence` rows.
- **GC never touches a pending row:** 100 never-acked sentinel rows survived
  every cadence (the second sanity gate), structurally matching
  `outbox_prune_sql`'s `dispatched_at IS NOT NULL` predicate.

### What this retires (R8c)

R8c's "write amplification, txn bloat, WAL" adjectives are now: +30 µs /
+500 B per single-row write; ×5–6 duration and ×2.2 WAL on bulk statements;
outbox growth bounded by `rate × cadence` under the shipped GC and unbounded
without it. The mitigation R8c asked for (per-registration emission) is
`wamn-vbl`, now with its price tag attached.

### Raw data

- `docs/ceilings-data/c2-trigger.csv`, `c2-bulk.csv`,
  `c2-growth-c{0,60,600}.csv` (extracted from the job log's
  `=== BEGIN CSV … ===` blocks).
- Full job log: session scratchpad `c2-record.log` (ephemeral; the CSVs +
  this reading are the durable record).
- Reproduce: `docs/build-and-test.md` § [EVT-C2 / wamn-z7b.2].

### Deferred (phase 2)

Payload/row-width axis (the trigger's WAL cost scales with the `to_jsonb`
after-image; one modest row shape measured here) — filed as a phase-2
measurement bead. Statement-level triggers with transition tables (the R8c
escape hatch if per-row invocation cost ever dominates) remain a deliberate
payload-shape decision, not a default.

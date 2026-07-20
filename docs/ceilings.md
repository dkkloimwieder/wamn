# Measured ceilings

> **§1.9a audit (2026-07-19): amendments are additive — base sound.**

> **Provenance banner (E6, 2026-07-19):** fixture-pod figures were measured under
> `fsync=off` + `synchronous_commit=off` — **shape-only, not citable externally**.
> Durable-commit re-measurement of the latency gates of record is tracked as
> wamn-dzhw. C7 measured the run queue, which survives the CDC pivot, so that
> number stays live; C2's subject (outbox capture) disappears at the D19
> Phase-2 teardown.

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
`wamn-gates queuebench --mode ceiling` (`deploy/gates/queuebench-ceiling-job.yaml`),
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
cluster on a single developer workstation, the shared `deploy/platform/postgres.yaml`
pod (postgres:18, **stock container config — default `shared_buffers`,
autovacuum, fillfactor; no tuning** — except `fsync=off` +
`synchronous_commit=off`, which the fixture pod has always run with
[deploy/platform/postgres.yaml], so every latency in this file excludes per-commit
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
(`deploy/gates/outboxbench-job.yaml`).

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
`deploy/platform/postgres.yaml` runs `fsync=off` + `synchronous_commit=off`, so every
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
(registration-driven per-entity emission) was sized against — an
*unregistered* table pays none of it, and the uniform all-tables plan
charges every table. (Superseded 2026-07-18: D19 v3 CDC capture retires the
trigger path — wamn-vbl closed, R8c closed; this stays as the cost record.)

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
without it. The mitigation R8c asked for (per-registration emission) was
`wamn-vbl` — closed superseded 2026-07-18 along with R8c itself: D19 v3's CDC
capture (`docs/event-plane-jetstream.md`) removes the amplification at the
source, and these numbers stand as the retired path's price tag.

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

## C-WAL-0 — pre-CDC WAL-volume baseline (wamn-l5i9.4)

**Date:** 2026-07-18 · **Git rev:** the `wamn-l5i9.4` commit (parent `a341d28`;
the campaign binary was built from this tree) · **Bench:** `wamn-gates walbench
--mode all` (`deploy/gates/walbench-job.yaml`).

**What is measured:** the *denominator* every later C-CDC WAL-delta claim
(`wamn-l5i9.14`) divides by — the WAL an application's writes generate BEFORE any
publication, replication slot, or non-default replica identity exists. Not the
outbox path (C2, retired): the plain 3.2 tenant floor for the poc-receiving
catalog (the real POC app model), written by `wamn_app` under the RLS floor. Two
legs: per-op WAL bytes across two row shapes (narrow `suppliers`, wide/TOASTy
`users`) × insert/update/delete, and a representative receiving-event write mix
(one transaction per event: a receipt + 3 receipt_lines, plus a quality_hold +
disposition on every 4th) → WAL bytes/event and bytes/s. WAL measured as
`pg_current_wal_insert_lsn()` deltas (WAL *generated*, exact under async commit —
the C2 lesson). The per-op batches bracket 1000 ops after a fresh CHECKPOINT, so
the first-touch full-page-image share amortizes consistently (the C2 discipline —
and the narrow INSERT lands at 253 B, matching C2's baseline INSERT exactly, an
independent cross-check of the instrument). The mixed leg brackets WAL **per
event**, not over the window: the insert LSN is instance-global, and a
window-long bracket on the *shared* fixture pod folds in other tenants' WAL (an
early run showed one 60 s window at ~5× another); a per-event bracket is a sub-ms
window whose sum excludes the idle gaps.

**Pre-CDC, made checkable:** the run asserts the measured DB has **0
publications, 0 replication slots, and every one of its 8 tables at DEFAULT
replica identity** before any measurement runs — so "pre-CDC denominator" is a
verified property, not an assumption. `wal_level = replica`.

**Environment:** the p0 reference class (same rig, pod, and co-location as C7/C2
above — `deploy/platform/postgres.yaml`, `fsync=off` + `synchronous_commit=off`, PG 18.4).
WAL byte counts are deterministic and unaffected by `fsync=off` (`wamn-dzhw`:
"WAL byte counts + growth curves unaffected either way" — confirmed here
byte-for-byte against an `fsync=off` throwaway PG); the p50s exclude per-commit
fsync (sub-0.1 ms). **Single-run record** (the C2 practice): no knee search a
one-sided stall can poison — the headline numbers are byte counts and medians.

### Per-op WAL (`cwal0-perop.csv`)

| shape | op | WAL/op | p50 |
|---|---|---|---|
| narrow (`suppliers`) | INSERT | 253 B | 0.053 ms |
| narrow | UPDATE | 311 B | 0.055 ms |
| narrow | DELETE | 205 B | 0.059 ms |
| wide/TOASTy (`users`, 6 KiB) | INSERT | 6969 B | 0.118 ms |
| wide | UPDATE | 13675 B | 0.105 ms |
| wide | DELETE | 6808 B | 0.058 ms |

A small business row costs **~200–310 B of WAL per single-row write**. A wide row
whose 6 KiB column TOASTs out-of-line (the wide leg genuinely TOASTed — 8.19 MB
of TOAST relation for 1000 rows) costs **~7 KB on INSERT and ~14 KB on UPDATE**:
the UPDATE writes a fresh out-of-line value while the old is retained for vacuum,
roughly doubling the INSERT. This width span is exactly why C-CDC splits narrow
vs wide — REPLICA IDENTITY FULL logs the *old row image* on UPDATE/DELETE, and
that added WAL scales with row width, so these are the two ends of the
denominator.

### Representative receiving-event load (`cwal0-mixed.csv`)

| offered rate | events | WAL/event p50 | WAL/event mean | WAL/s |
|---|---|---|---|---|
| 20/s | 1200 | 1280 B | 1761 B | 35 KB/s |
| 50/s | 3000 | 1280 B | 1581 B | 79 KB/s |

A **typical receiving event** (a receipt + 3 lines, one transaction) generates
**~1.3 KB of WAL** — the identical p50 at both rates. The mean lifts to
~1.6–1.8 KB because one event in four also opens a quality hold and records a
disposition. The app-load WAL rate scales ~linearly with the event rate
(~1.3–1.8 KB × events/s); this is the "representative app load" bytes/s baseline
the per-org capacity model (§8) sizes retention and disk growth against.

### The denominator

Every C-CDC WAL-delta figure (`wamn-l5i9.14`: "WAL delta under FULL identity per
table class") divides by these pre-CDC numbers. Recording them *before* the
publication/slot lands (the `wamn-l5i9.9` → `wamn-l5i9.4` bd dependency keeps that
ordering) is what makes the delta attributable to CDC and nothing else. The one
methodology note that carries forward: the insert LSN is instance-global, so on a
shared pod per-op / per-event brackets stay clean while a window-long bracket does
not — C-CDC's window measurements will want the same per-event discipline (or a
dedicated cluster).

### Raw data

- `docs/ceilings-data/cwal0-perop.csv`, `cwal0-mixed.csv` (extracted from the job
  log's `=== BEGIN CSV … ===` blocks).
- Reproduce: `docs/build-and-test.md` § [EVT-C-WAL-0 / wamn-l5i9.4].

## C-MAT — materializer deliveries→enqueue (wamn-l5i9.17, first numbers)

**Provenance: LOCAL, DEBUG-build harness, release guest wasm, in-process
runtime (matbench), single-node throwaway NATS + postgres:18 (fsync=off).**
These are *shape* numbers, not ceilings: the drain wall-clock is dominated by
the sweep structure (per-registration fetch long-polls of `fetch_ms=1500` and
`ceil(burst/batch)+2` sweeps), not by decide/enqueue cost. The in-cluster
C-MAT campaign re-measures on the real rig with tuned batch/fetch windows.

| metric | value | notes |
|---|---|---|
| fixture tape (7 events × 4 registrations) | 8 fires, 4+3+1+2 skips/refusals | one guest run, 2 sweeps, ~4.5 s |
| burst drain | 200 events → 600 runs in ~10–14 s | 3 servings/event; ~15–20 deliveries/s, ~44–61 enqueues/s |
| duplicate storm (full redelivery, 207 events × 3) | 608 `ON CONFLICT` collisions, **0 new rows** | consumers deleted server-side; exactly-once holds |

- Reproduce: `docs/build-and-test.md` § [EVT-MAT / wamn-l5i9.17].

## C-E2E — outbox-vs-CDC before/after (wamn-l5i9.22, first numbers)

**Provenance: LOCAL-THROWAWAY, DEBUG-build host binaries, RELEASE guest wasm,
in-process runtime (e2ebench), single-node throwaway NATS (nats:2, 1 replica) +
postgres:18 (`fsync=off`, `synchronous_commit=off`, `wal_level=logical`),
`disp_poll_ms=50`, `fetch_ms=100`. Machine: `local-throwaway` (a developer
workstation shared with a kind cluster + other containers — a NOISY rig, see
C7's one-sided-noise caveat).** These are **shape** numbers for METHODOLOGY
VALIDATION, not ceilings — the absolute latencies are dominated by the two
tunable pacing terms (the dispatcher's 50 ms poll cadence on the old path; the
materializer's 100 ms per-registration fetch long-poll on the new path), so a
re-tune moves them. The in-cluster release-image `e2ebench` job (below)
re-measures on the reference rig; the rows OF RECORD come from there.

**Date:** 2026-07-20 · **Git rev:** the `wamn-l5i9.22` commit (parent
`322cec8`) · **Bench:** `wamn-gates e2ebench --phase all`
(`deploy/gates/e2ebench-job.yaml`), run twice back-to-back for repeatability.

**What is measured (each vs BOTH real paths at identical write load, one writer
program):** ONE process composes both paths (the cutbench substrate). OLD path
= a `wamn_app` write to an `old_*` table carrying the REAL
`Migration::outbox_triggers` → the real `wamn_dispatcher` engine
(poll/match/fire/enqueue), run id `{flow}:outbox:{seq}`. NEW path = a `wamn_app`
write to a trigger-free `new_*` table → the embedded real `wamn-cdc-reader`
(one `pg_walstream` session) → JetStream → the real `materializer.wasm` guest,
run id `{flow}:evt:{stream_seq}`. Old-arm flows carry NO live CDC registration
(the dispatcher fires them); new-arm flows carry live registrations (the
dispatcher yields them — moot, a trigger-free table produces no outbox rows —
and the materializer fires them). Runs attribute by the disjoint run-id
namespace. The commit instant is `clock_timestamp()` returned by the writing
INSERT; the enqueue instant is `run_queue.enqueued_at` (server `now()` at the
fire/enqueue txn) — **both the same Postgres wall clock** (the throwaway PG
shares the host kernel clock, so there is no client↔server skew), latency =
enqueued − commit. **Error bound ≈ ±1–2 ms:** `clock_timestamp()` is evaluated
a hair before the autocommit finalises (slight over-count) and `enqueued_at` is
the enqueue txn's *start* (slight under-count) — the two biases partly cancel
and are dwarfed by the 100-ms-scale signal.

Only structural sanity asserts gate (so the numbers are trustworthy): every
write on each arm produced exactly its expected run count — 120/120 + 600/600
(distribution), 24×{1,5,20} → {24,120,480} runs per arm (fan-out) — and both
burst backlogs built then drained. All held on both runs.

### (a) commit→run-start distribution (`ce2e-dist.csv`, `ce2e-dist-hist.csv`)

| path | rate | p50 | p90 | p99 | mean |
|---|---|---|---|---|---|
| old (outbox) | 10/s | 27 ms | 48 ms | 53 ms | 26 ms |
| new (CDC) | 10/s | 129 ms | 315 ms | 327 ms | 163 ms |
| old (outbox) | 50/s | 30 ms | 52 ms | 59 ms | 29 ms |
| new (CDC) | 50/s | 153 ms | 229 ms | 267 ms | 154 ms |

At steady state the **OLD outbox path delivers a run to the queue FASTER** than
the new CDC path on this config: the old-path latency is a near-uniform draw
over the 50 ms dispatcher poll interval (p50 ~27–30 ms, p99 ~53–67 ms), while
the new path pays WAL-decode + a JetStream hop + the materializer's 100 ms
fetch long-poll (p50 ~130–165 ms, p99 ~265–365 ms). **CDC's win is NOT
steady-state latency** — it is the app-write decoupling (a) and the removal of
the synchronous trigger tax (below). Both terms are pure config: a tighter
`fetch_ms` and a batched pull pull the new-path p50 down; a slacker
`disp_poll_ms` (production min is **250 ms**, 5× the 50 ms used here) pushes the
old-path latency UP — real production old-path p50 sits near ~125 ms, above the
new path measured here. Run-to-run: old p50 within 2 ms; new p50 varied
129↔166 ms (the fetch long-poll boundary is noise-sensitive).

### (b) fan-out 1→N — the headline (`ce2e-fanout.csv`)

| path | N | app-txn p50 | app-txn p99 | commit→last-run p50 | p99 |
|---|---|---|---|---|---|
| old (outbox) | 1 | 0.73 ms | 1.67 ms | 24 ms | 50 ms |
| new (CDC) | 1 | 0.37 ms | 0.99 ms | 152 ms | 350 ms |
| old (outbox) | 5 | 1.27 ms | 5.88 ms | 29 ms | 54 ms |
| new (CDC) | 5 | 0.76 ms | 4.19 ms | 204 ms | 352 ms |
| old (outbox) | 20 | 1.06 ms | 1.63 ms | 41 ms | 129 ms |
| new (CDC) | 20 | 0.65 ms | 1.36 ms | 314 ms | 571 ms |

**The headline correction to the plan's premise.** The plan text sized the
before/after as *"N outbox rows written INSIDE the app txn (write amplification
on the app transaction)"*. Measured, that is **not how the shipped path
behaves**: `Migration::outbox_triggers` emits ONE per-table `AFTER … FOR EACH
ROW` trigger that writes **one** outbox row per write regardless of how many
flows subscribe — the fan-out to N runs happens entirely POST-commit, at the
dispatcher. So **app-transaction commit latency does not scale with fan-out N**
on either path (old ~0.7–1.3 ms, new ~0.4–0.8 ms, flat across N=1/5/20 within
noise). The whole app-side old-vs-new difference is the **constant** outbox
trigger tax — visible here as new-CDC running a consistent ~0.3–0.5 ms below
old-outbox per write (the C2 "+30 µs / +500 B" single-row tax, buried inside a
sub-ms round trip on this fast local loop; it shows sharply in WAL/bulk, not in
single-row commit latency). What DOES scale with N is **commit→last-run**: old
scales gently (24→29→41 ms) as the dispatcher fires N runs from one outbox row;
new scales steeply (152→204→314 ms, and up to ~950 ms at N=20 in run 2) because
the materializer fetches its N registrations' consumers **serially**, each with
its own ~100 ms long-poll — the per-registration serial fetch is the new path's
fan-out cost and the clearest tuning target (parallel/multiplexed consumer
fetch).

### (c) burst — 10× spike (`ce2e-burst-depth.csv`, `ce2e-burst-applat.csv`)

Steady 40/s, spike ×10 (400/s) for 5 s, then drain observed while steady
continues. **App-write latency was NOT degraded during the spike on either
path** — through the 5 s of ~400 writes/s both arms held p50 ~0.6 ms / p99
~1.8 ms (old-outbox a steady ~0.08 ms above new-CDC, the trigger tax again),
with a small SHARED post-burst p99 blip to ~13 ms (checkpoint/vacuum, both arms
equally). Lag depth + drain were **heavily run-to-run noise-dominated** (the
shared rig):

| run | old peak backlog | old drain | new peak pending | new drain |
|---|---|---|---|---|
| run 1 (clean) | 84 rows | 0.9 s | 9 msgs | 0.1 s |
| run 2 (noise-poisoned) | 438 rows | 14.1 s | 585 msgs | 2.9 s |

Both paths absorb the 10× spike and drain; the new path drains faster (batched
pull vs one-row-per-poll fire). The absolute depths/drains are not citable from
this rig — they swing ~5–15× between back-to-back runs (host contention on one
disk, the C7 caveat) — but the **mechanism** is instrumented: outbox unfired
backlog (`dispatched_at IS NULL`) for the old path, JetStream consumer
`num_pending` for the new. The committed CSV is run 1 (the clean run); run 2's
poisoned depths are recorded here as the variance envelope.

### What this says about the design (curves, not a verdict)

The CDC plane is **not** a steady-state-latency win at these settings — it is
slower to first-enqueue than a tightly-polled outbox. Its case rests elsewhere,
and this bench isolates where: (1) it removes the synchronous per-write trigger
tax from the app transaction (constant, small per row, but paid by every write
forever and unbounded on bulk — C2); (2) app-write latency is decoupled from
consumer lag (burst absorption held app p99 flat); (3) the fan-out cost moves
off the app txn to an async consumer. The open tuning question it surfaces: the
materializer's serial per-registration fetch makes new-path fan-out latency
grow with N — the in-cluster campaign should sweep `fetch_ms`/batch and a
parallel-fetch variant.

### Raw data

- `docs/ceilings-data/ce2e-dist.csv`, `ce2e-dist-hist.csv`, `ce2e-fanout.csv`,
  `ce2e-burst-depth.csv`, `ce2e-burst-applat.csv` (run 1 — the clean run;
  extracted from the job log's `=== BEGIN CSV … ===` blocks).
- Reproduce: `docs/build-and-test.md` § [EVT-C-E2E / wamn-l5i9.22].

### Campaign of record (in-cluster, release image, 2026-07-20)

Provenance: `env=in-cluster-kind build=release-host-binaries release-guest-wasm
pg=fixture-pod-postgres:18(fsync=off,synchronous_commit=off,wal_level=logical)
nats=evt-nats(3-node,R3-cluster;bench-stream-R1) disp_poll_ms=50 fetch_ms=100
machine=kind-wamn/postgres-colocated — CAMPAIGN OF RECORD` (job
`deploy/gates/e2ebench-job.yaml`; all completeness gates PASS: every write
produced exactly its N runs on BOTH paths in every phase).

Commit→run-start (N=1):

| rate | old p50 / p90 / p99 | new p50 / p90 / p99 |
|---|---|---|
| 10/s | 25.5 / 45.4 / 49.6 ms | 184.3 / 290.3 / 385.2 ms |
| 50/s | 26.6 / 47.8 / 53.0 ms | 157.0 / 238.6 / 310.8 ms |

Fan-out 1→N (24 events per N):

| N | app-txn p50 old / new | commit→last-run p50 old / new |
|---|---|---|
| 1 | 0.495 / 0.212 ms | 19.4 / 166.2 ms |
| 5 | 0.495 / 0.205 ms | 29.6 / 166.1 ms |
| 20 | 0.454 / 0.229 ms | 30.8 / 185.0 ms |

Burst (10× over 40/s steady, 5 s spike): old peak backlog 12 rows, drained
268 ms after spike end; new consumer pending never sampled >0 at 200 ms cadence
(drain outpaced the sampler), settled ≤66 ms after spike end.

Readings: (1) the **trigger tax is directly visible and constant** — ~0.25–0.3
ms per app write (old 0.45–0.50 ms vs new 0.21–0.23 ms p50), flat in N on both
paths (the fan-out premise correction from the local runs holds on the record
rig: the old path writes ONE outbox row per write; fan-out was always
post-commit). (2) A tightly-polled outbox still wins steady-state first-enqueue
(~26 ms vs ~157–185 ms p50 at `disp_poll_ms=50` / `fetch_ms=100` — both pacing
knobs, recorded in provenance; production dispatcher minimum poll is 250 ms).
(3) On the release build the new-path fan-out slope is nearly FLAT (166→185 ms
p50 from N=1→20) — the steep serial-fetch growth in the local debug runs was
largely a debug-guest artifact; the `fetch_ms`/batch sweep (wamn-l5i9.64)
remains the tuning lever but is not the cliff the local shape suggested.
(4) Burst absorption: the CDC path absorbs a 10× spike without measurable
consumer lag at 200 ms sampling while the app path stays flat — the
write-decoupling claim, observed.

Raw rows: `docs/ceilings-data/ce2e-record-{dist,dist-hist,fanout,burst-depth,burst-applat}.csv`.

### Deferred (in-cluster / follow-ups)

A `fetch_ms`/batch sweep + a parallel-consumer-fetch variant for new-path
fan-out (wamn-l5i9.64 — softened by the record run's flat slope, reading 3
above); the app-CRUD-p99-under-capture C-INTERFERENCE bench (sibling, D19 §7).
The before/after record predates EVT-TEARDOWN (l5i9.19) as required — the old
path was alive for every row above.

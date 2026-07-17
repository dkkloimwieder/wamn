# Event Plane Design — JetStream behind the outbox (candidate D19, v2)

**Status:** comprehensive design for sign-off. v2 supersedes v1: adds the
wasmCloud-alignment section (§9), replaces functional gates with a **ceiling
characterization program** (§10 — measurement, not pass/fail; we want to know
the numbers before attaching verdicts to them), and adds a provenance ledger
(§11) after v1 shipped an unlabeled extrapolation as a threshold.

**v2.1 (2026-07-17, review amendments):** run identity is **source-derived**
(outbox seq via `Nats-Msg-Id`, never the stream seq) — closes the
beyond-dedupe-window duplicate hole and makes the Phase B cutover idempotent
across old/new paths (§5/§6); outbox GC correctly stated as **missing today**
(wamn-d8v is a relay prerequisite, §5); 5.10 claim-check is a **scope change**,
not just an unpark (§4); C4 gains an asset-count dimension (§10); stream naming
re-keyed on D18 recovery domains (§4); materializer takes over the wake
doorbell (§6); **C1 re-sequenced before C4** behind a decision checkpoint
(§10/§14); MQTT de-emphasized — no current driver, external ingest assumed
HTTP for now (dkk, 2026-07-17); §9's upstream references labeled with their
real status (in-flight PRs, not landed surfaces).

**One-line shape:** the outbox stays exactly where it is; everything *after* it
changes. Events get broker semantics (stored once, fan-out by cursor, retention,
replay); execution keeps database semantics (leases, partition ownership,
crash-evidence attempts, co-transacted write-ahead audit).

---

## 1. Principles this design is built on

1. **No broker can join a Postgres transaction.** Atomic event capture requires
   the outbox trigger in the customer's commit log. The outbox is a
   prerequisite of JetStream, not an alternative to it (dual-write orderings
   both fail: lost events or phantom events).
2. **wamn journals intentions and progress, never effects.** The stream retains
   *facts* ("this row changed"); run state retains *execution position + step
   I/O*. Nothing anywhere is an undo log. Replay is forward re-execution and
   re-fires side effects (§8).
3. **Events ≠ execution.** JetStream-as-run-queue stays rejected (D3 grounds:
   no co-commit with write-ahead audit, no crash-evidence attempts, no
   partition leases). Streams feed the queue; they don't replace it.
4. **Blast radius follows the org** (T2/R8b doctrine) — streams, accounts,
   retention, and residency are org-scoped.
5. **Prefer ecosystem-standard surfaces** (D17). This subsystem needs zero
   custom WIT (§9).

## 2. Architecture

```
customer txn ──▶ outbox row                     app DB (unchanged)
                    │
        RELAY  (dispatcher-owned, native)       poll → publish → ack-mark → GC
                    ▼
        JetStream stream  EVT_<org>_<domain>    data-plane NATS cluster
                    │  durable consumer per subscribing flow (filtered subjects)
                    ▼
        MATERIALIZER  (v1 native; target: component per org — §9)
                    │  write-ahead + ON CONFLICT enqueue (deterministic run_id)
                    ▼
        Postgres run queue ──▶ run-workers      execution plane (unchanged)
```

Paths that never touch the stream: **cron** (dispatcher → queue direct),
**sync webhooks** (direct dispatch, D15), **doorbells** (control-plane NATS
core, fire-and-forget — a wake hint is not an event). Future high-rate
external ingest (**no current driver — assume HTTP for now**, dkk 2026-07-17;
MQTT if the industrial tranche materializes) would publish onto the same
streams — prepared rails rather than a new design.

## 3. Infrastructure: dedicated data-plane NATS

- Separate JetStream-enabled NATS cluster (3 nodes, file storage, R3 streams
  for prod domains; R1 acceptable for dev/trials). The control-plane NATS
  (operator, doorbells) stays core-only and untouched.
- Rationale: wasmCloud's own control/data split (the Helm chart's separate NATS
  URLs exist for exactly this); JetStream raft/storage failure modes must not
  reach the operator or doorbells; independent sizing and upgrade cadence.
- This is the design's main standing cost: a second durability domain with its
  own on-call (raft health, disk, consumer lag, retention).

## 4. Stream topology

- **One stream per org per recovery domain**, named by the **D18
  recovery-domain id**: `EVT_<org>_<domain>` — the seeded env policies yield
  `prod`/`dev`; an env whose policy says `shared-with` lands on the shared
  domain's stream (T2 canary → the prod stream); a policy with recovery-domain
  `own` (T4 regulated canary) gets its own. Never a literal env enum in the
  name — D18 just retired that encoding; naming keys on
  `env_policies.recovery_domain`. Retention/residency/blast-radius align with
  domains customers already have. Trials: shared `EVT_trials`,
  subject-isolated.
- **Subjects:** `evt.<org>.<project>.<env>.<table>.<op>`
  (op ∈ insert|update|delete). Reserved for future ingest:
  `evt.<org>.<project>.<env>.mqtt.<topic…>` (no current driver).
- **Message identity:** `Nats-Msg-Id = <project_env>:<outbox_seq>` — the
  **source identity**, deterministic; used by the dedupe window (§5) and —
  load-bearing — it, not the stream sequence, derives run identity (§6).
- **Payload:** outbox after-image JSON, capped (proposed 256 KiB — a knob, and
  C4 measures the cost curve that should set it). Oversize → **claim-check**:
  relay spills to the payload store (5.10) and publishes a `payload-ref`.
  **5.10 is hereby a prerequisite** of event-plane GA and forces its backend
  decision — and a **scope change**, not just an unpark: 5.10 as specced is
  *run-scoped*, but claim-check objects are *pre-run* and must live for the
  full retention window (retention IS the replay horizon, and replay re-reads
  the refs). Event-scoped objects with GC coupled to stream retention, or
  replay silently breaks on every claim-checked event (wamn-sdp carries the
  note).
- **AuthZ:** per-org NATS accounts (minimum: per-org subject-prefix
  permissions). Consumer credentials are org-scoped — a leaked credential reads
  one org's events, not the platform's.
- **Retention:** limits-based, per-tier knob. **Retention IS the replay
  horizon** — a billable, capacity-planned number (C4 measures growth rates so
  tiers can be priced from data).

## 5. The relay (dispatcher evolution)

The outbox poller changes destination — from "write N run rows" to "publish
once":

1. Poll undispatched rows per project-env in `seq` order (single-owner per
   project via existing dispatcher ownership → publish order preserved).
2. Publish with `Nats-Msg-Id`; await JetStream ack. Batching + pipelining depth
   are knobs (C3 characterizes the curve) — with an **ordering constraint**: a
   failed/timed-out ack halts the pipeline for that project-env and resyncs
   from the last acked seq, never retrying one publish while later ones land.
   Run identity and ordering survive out-of-order arrival regardless (both are
   source-derived, §6), but resync-on-error keeps the stream itself in outbox
   order — what a replay reader sees — and bounds the interleavings the
   functional verification has to reason about.
3. Mark dispatched (same DB as the outbox — this write remains
   single-database).
4. GC — **does not exist yet.** Verified (wamn-d8v): ack only sets
   `dispatched_at`; nothing prunes, the outbox grows without bound. wamn-d8v
   (janitor-colocated pruner + registration-driven trigger emission) is a
   **prerequisite of the relay** and of C2's growth-vs-GC measurement.

**Crash orderings:** publish → crash → repoll → republish → the **dedupe
window** (set ≥ 2× outbox redelivery horizon, e.g. 10 min) drops the duplicate.
The window is time-bounded; beyond it duplicates CAN reach the stream — and a
beyond-window republish is not an edge case: any relay outage longer than the
window between publish-ack and mark-dispatched (a bad deploy, a stuck restart)
guarantees one, arriving as a *new* stream message with a *new* stream seq.
Layered defense, stated plainly: dedupe window = fast path; the materializer's
`ON CONFLICT` (§6) = the actual guarantee — which it only is **because run
identity derives from the source seq**. Keyed on stream seq (v2's bug), the
beyond-window duplicate would mint a fresh run id and sail through the
conflict check.

**Degradation:** JetStream down → relay stalls, outbox accumulates (alert:
oldest-undispatched age), row-event flows are *delayed never lost*; cron, sync
webhooks, and in-flight execution unaffected. Strictly better than any
dual-write design; goes in the ops runbook verbatim.

**Why the relay stays native:** it holds multi-org DB credentials and polls
every org's outboxes — control-plane work by nature, the same justified
exception the dispatcher already carries in the posture doc.

## 6. Consumers and the materializer

- **One durable consumer per subscribing flow**, subject-filtered. Consumer
  count = per-org flow count; consumers carry raft state → per-org quota (C4
  measures the actual per-consumer cost so the quota is a number, not a vibe).
- **Materializer semantics** (the exactly-once core): receive delivery →
  write-ahead row + queue row with deterministic **source-derived**
  `run_id = <flow>:outbox:<seq>` (outbox seq from `Nats-Msg-Id`; the queue is
  per-project-DB, so the seq is already project-env-scoped) under
  `ON CONFLICT DO NOTHING` → ack. Crash before ack → redelivery → conflict
  no-op → ack. Never the stream seq: a beyond-dedupe-window duplicate carries
  a new stream seq and would mint a duplicate run (§5). Source-derived, the
  same outbox row collapses to the same run id whatever path it took —
  redelivery, window expiry, or a republished relay batch. **Cutover bonus:**
  this is byte-identical to the identity the dispatcher writes today
  (`{flow}:outbox:{seq}`), so during Phase B's transition the old N-row path
  and the materializer collide on the same rows — the cutover is exactly-once
  *across* paths, by construction, and dual-running is safe rather than
  double-firing.
- **Ordering:** the run ordering key embeds the **outbox seq** (from
  `Nats-Msg-Id`), not the stream seq — queue order matches *source* order per
  project-env even when publish retries or redeliveries interleave stream
  arrival. The stream seq is bookkeeping (consumer cursor position), never
  identity or order. For `partitioned(key)` flows, the key is extracted from
  the payload at materialization; **R6's `blocking` decision is load-bearing
  here** — this design hard-depends on it. Decide R6 before Phase B.
- **Doorbell handoff:** today the dispatcher rings the wake doorbell after
  enqueue; post-cutover the **materializer takes that over** (control-plane
  NATS core hint after the enqueue commit) — otherwise async dispatch latency
  silently degrades to the reconciliation sweep (30 s–5 min).
- **Backpressure:** materializer enqueue rate is bounded by the queue's ceiling
  (C7); consumer lag absorbs bursts by design — lag is the shock absorber, and
  C6 measures how deep it gets at what cost.

## 7. What this replaces / keeps / adds

| Concern | Today | Event plane |
|---|---|---|
| Atomic capture | outbox trigger | **unchanged** |
| Fan-out (N flows, 1 event) | dispatcher writes N run rows | stored once; N cursors |
| Retained events / replay | absent (outbox GC'd) | retention window; re-drive |
| Doorbells | NATS core hint | **unchanged** |
| Cron / sync webhooks | direct paths | **unchanged** — never touch the stream |
| Run queue / run state | Postgres | **unchanged** — fed by, not replaced |
| MQTT (future) | undesigned | same streams, same machinery |

## 8. Replay — teeth showing

Re-drive = new consumer at a past position/time for a chosen flow. Replay runs
get a distinct id namespace (`<flow>:replay:<replay_id>:<seq>`, seq = the
replayed message's *source* seq from `Nats-Msg-Id`) so they never collide with
originals' dedupe, plus `replay=true` in run state for audit/UI.

**Loud, permanent caveat:** replay **re-executes side effects** (principle 2).
Idempotency keys are forwarded; external dedupe is the external system's job.
Opt-in per flow, permission-gated, audited. C8 measures drain rates and
live-consumer interference so replay windows can be sized honestly.

## 9. wasmCloud-model alignment

This subsystem is the most wasmCloud-idiomatic thing in the platform, and one
piece of it resolves a problem the adoption plan couldn't:

1. **NATS-as-data-plane is wasmCloud's intended architecture** — not a
   workaround. The Helm chart's separate control/data NATS URLs exist for this
   exact shape; wasmCloud 2.0 removed JetStream from *scheduling*, never from
   the data plane.
2. **Zero custom WIT.** The guest-facing surface, when it arrives, is
   `wasmcloud:nats` (#5065 — **a draft PR, open and unmerged** as of
   2026-07-17; an in-flight surface, not a landed one — labeled as such in
   §11) — the first major wamn subsystem needing no `wamn:*` exception under
   D17's interface policy.
3. **The materializer is the platform's best component candidate — better than
   the run-worker.** Its shape: consumes deliveries it is *handed*; needs
   exactly two capabilities (`wasmcloud:nats` in, `wamn:postgres` out — both
   WIT imports); stateless between deliveries; partitions naturally per org.
   That is precisely the "long-lived trigger service" #5336 is building
   (**open PR, approved,** as of 2026-07-17: routes inbound messages to
   long-lived services instead of per-message instances) — and it is
   **invoked-shaped**: the host's messaging provider consumes the stream
   (pull, per the modern JetStream client API — push consumers are legacy)
   and *invokes* the service per delivery. That sidesteps the impedance that
   makes the run-worker's migration uncertain — the run-worker *owns a claim
   loop*, and a component can't be a long-poller. Target state: **component
   per org,
   `wasmcloud:nats` + `wamn:postgres` imports, scheduled via
   `WorkloadDeployment`, routed as a #5336 trigger service** — the first wamn
   service fully wasmCloud-idiomatic end to end, and the low-risk proving
   ground for the adoption plan's Phase 4 (prove the model on the materializer
   before betting the runner on it). v1 ships it native inside the dispatcher;
   the componentization is an adoption-plan phase, spiked against the #5336
   branch per the fork-first discipline.
4. **The relay's native status is a recorded exception** (multi-org
   credentials; §5), joining the dispatcher's existing posture-doc row.
5. **Posture C on-ramp, not a detour:** per-org streams pinned to leaf nodes
   later *is* the edge/residency story. Building the event plane now is
   pre-paying the lattice era's data plane.

## 10. Ceiling characterization program (measurement, not gates)

**Philosophy:** these benches produce **curves and ceilings, not verdicts**. No
pass/fail thresholds are attached in this document — the owner attaches
decision rules after the numbers exist. Output of the program: a **capacity
model** ("one org sustains X events/sec steady, Y burst for Z minutes, at W GiB
retention/day, with app-path p99 impact of V%") published to
`docs/ceilings.md`, every figure carrying its measurement date, environment,
and raw-data pointer. Functional verification (crash orderings, redelivery,
ordering) is deliberately out of scope here and is planned at implementation
time as beads.

**Environment & methodology (applies to all of C1–C9):**
- Fixed reference environment, documented once: 3-node cluster (same class as
  p0 runs), dedicated data-plane NATS (3× nodes, file storage, R3), one T2-shaped
  org cluster (CNPG, same instance class as production default), instrumented
  with the existing OTel pipeline.
- Load generation extends `wamn-gate-harness` with a **ceiling mode**: ramp
  profiles (step: 1 min/level; find-knee: binary search on the level where p99
  doubles or lag diverges), sustained soaks (30 min at 80% of knee), burst
  profiles (10× for 60 s over baseline).
- Metrics captured uniformly: p50/p99/p999 latency per stage, CPU + memory per
  process, Postgres: WAL bytes/sec, dead tuples, autovacuum wall-share,
  checkpoint pressure; NATS: publish acks/sec, consumer lag, raft traffic, disk
  growth; end-to-end: commit→run-start distribution.
- Every run records: git rev, config knobs, raw CSVs. Curves over single
  numbers, always.

**C1 — Retained-events-table-in-Postgres ceiling (the §13 alternative,
finally measured).** Build the minimal alternative: `events` table (append),
per-flow cursor rows, cursor-claim loop. Matrix: append rate 250 → 500 → 1k →
2k → 5k → knee events/sec × consumers 1/5/20 × payload 1/16/64 KiB, co-resident
with app CRUD at fixed rate on the same instance. Measure: app-path p99 delta,
bloat growth, WAL share, cursor-claim throughput per consumer, end-to-end
latency. **This replaces v1's fabricated "1–2k events/sec/org" with a measured
knee** and becomes the honest crossover criterion for the retreat option.

**C2 — Outbox trigger overhead.** The cost the *customer* pays: single-row
write latency with/without trigger; bulk single-statement UPDATE of 1k/10k/100k
rows (write amplification curve, txn duration, WAL) — quantifies R8c instead of
adjectives. Also: outbox table growth vs GC cadence under sustained load.

**C3 — Relay ceiling.** Poll→publish→ack pipeline: batch size (1/64/256/1024) ×
pipelining depth (1/8/32) × payload size, per project-env and across 10/100
project-envs on one relay instance. Measure events/sec per relay, outbox-age
under load, CPU per 1k events. Establishes relay shard count as a function of
org activity.

**C4 — JetStream ceilings on our reference infra.** (a) Publish: msgs/sec vs
payload size × R1/R3 × file storage, ack latency distribution. (b) Delivery:
per-consumer throughput; aggregate vs consumer count 10/100/500 on one stream —
the per-consumer raft/memory cost that turns the per-org quota into a number.
(c) Dedupe-window cost: publish throughput with window 2 s/10 min/1 h. (d)
Storage: bytes/event on disk (overhead factor), growth/day at reference rates —
prices the retention tiers. (e) Recovery: node kill mid-load → time to R3
re-heal, publish stall duration. (f) **Asset-count scaling** — (b) measures
consumers on *one* stream, but per-org streams × per-flow consumers means
cluster-wide asset count grows with org count, each R3 stream/consumer a raft
group, and JetStream's meta-raft layer is the known many-assets pain point:
measure cluster behavior (meta-raft convergence, restart time, memory) at
100/1k/5k streams with realistic consumer fan-out — this, not (b) alone, sets
the orgs-per-cluster number.

**C5 — Materializer ceiling.** Deliveries → `ON CONFLICT` enqueue: rows/sec
per materializer vs batch-ack size; conflict-path cost (100% duplicate storm —
the redelivery worst case); combined with C7 to find whether stream delivery or
queue insert is the binding constraint.

**C6 — End-to-end and fan-out curves.** Commit→run-start distribution at 10%/
50%/80% of knee load; fan-out amplification 1 event → 1/5/20 flows measured
against the *old N-row path* at identical load (storage, latency, dispatcher
CPU) — the before/after that justifies or indicts the whole design in one
chart. Burst behavior: 10× spike → lag depth, drain time, app-path impact
during drain.

**C7 — Run-queue ceiling, folklore retired.** The D3 estimate ("~1–5k
transitions/sec") finally measured on reference hardware: queuebench ceiling
mode — transitions/sec at knee vs fillfactor/batch-claim size/autovacuum
settings; bloat curve over a 1M-run soak. Every downstream capacity statement
inherits this number.

**C8 — Replay drain.** Re-drive rate (events/sec) for a 1 M-event window;
interference with live consumers on the same stream (lag delta); materializer
conflict-storm behavior when replaying an already-run window into the replay
namespace.

**C9 — Interference matrix (the co-residency truth table).** App CRUD p99
delta while each of C1(alt)/C2/C5/C7 runs at 80% of its knee on the shared org
instance — extends the runtime-DB-split interference bench so *both* open
placement questions (events plane and runtime DB) are decided from one
consistent dataset.

**Sequencing of the program (v2.1 — C1 promoted ahead of C4):** C7 and C2
first (they exercise only what exists today — no new infra; C2's growth-vs-GC
sub-measure waits on wamn-d8v). **C1 next, before any new infrastructure** —
the retained-events knee is the cheapest experiment and the decision-relevant
one. Then a **decision checkpoint** (the D19 row resolves): if C1's knee
comfortably clears target scale and no external driver has arrived (a design
partner needing fan-out/replay, or high-rate ingest), §13 is the mainline and
everything below defers — the standing NATS cluster is this design's main
cost (§3), and it should not be stood up to find that out. Only past the
checkpoint: C4 (data-plane NATS in staging, characterized bare), C3/C5/C6
with the Phase A prototypes, C8/C9 alongside. Shadow mode is then the load
rig for the JetStream-specific rows only.

## 11. Provenance ledger (numbers hygiene)

| Claim | Status |
|---|---|
| Plugin ~2k qps, p99 <10 ms | **measured** (S2, p0-results) |
| Dispatch p99s (write-ahead 1.11 ms, fast 361 µs) | **measured** (queuebench) |
| Queue ~1–5k transitions/sec | **estimate** (D3 folklore) → C7 measures |
| v1's "events table sufficient below 1–2k events/sec/org" | **fabricated extrapolation — retracted**; C1 measures the knee |
| 256 KiB payload cap | **proposed knob** → C4a informs |
| Dedupe window 10 min | **derived** (2× redelivery horizon) → C4c prices |
| Per-org consumer quota | **unknown** → C4b/C4f set |
| MQTT plant rates "10k+ msgs/sec bursts" | **industry-typical assumption**, not customer-measured; **de-scoped as a driver 2026-07-17** (assume HTTP ingest) |
| `wasmcloud:nats` surface (#5065) | **in-flight draft PR** — open, unmerged (checked 2026-07-17) |
| #5336 trigger-service routing | **open PR, approved** — unmerged (checked 2026-07-17) |

Rule going forward: no number enters a wamn design doc without one of these
labels.

## 12. Costs and sharp edges (standing)

Second durability domain + on-call (raft, disk, lag, retention); two-layer
exactly-once replaces one-transaction proofs (more interleavings — functional
verification at implementation time); consumer sprawl bounded by C4b's quota;
retention economics now billable and capacity-planned; R6 load-bearing for a
second subsystem; 5.10 promoted to prerequisite; claim-check adds a payload-
store dependency to event delivery.

## 13. The alternative on record: retained-events table in Postgres

Zero new infrastructure; same durability domain; sufficient below **C1's
measured knee** (not v1's invented number). JetStream's honest case, in order
(**re-ranked 2026-07-17** — MQTT out of scope, no current driver): push
delivery + consumer semantics come free rather than hand-rolling a worse
JetStream inside Postgres; NATS is already strategic and `wasmcloud:nats` is
the (in-flight) ecosystem surface; high-rate ingest that must not share the
customer's instance, *when* it arrives. If §12's ops cost ever binds, this is
the documented retreat — outbox, subject scheme, and materializer semantics
survive the retreat unchanged. And below C1's knee with no external driver,
this is not the retreat but the **default** (§10 checkpoint): the JetStream
build-out is what needs the argument, not the Postgres table.

## 14. Phasing (unchanged from v1, plus the program)

- **Phase 0 (now, cheap):** freeze schemas on paper — subjects,
  `Nats-Msg-Id` format, **source-derived** `run_id` format, payload cap +
  claim-check policy, D18-keyed stream naming (done in v2.1 + the D19
  decision-table row); decide R6 (wamn-1d4 — the ladder's 5.11 needs it
  regardless); record the 5.10 scope change (wamn-sdp); run C7 + C2 (C2's GC
  sub-measure after wamn-d8v).
- **Phase 0.5 (checkpoint):** C1 prototype + measured knee; **D19 decision**
  on C1/C7/C2 numbers + driver status (§10). Everything below is gated on
  this checkpoint.
- **Phase A (shadow):** data-plane NATS in staging; relay dual-publishes; the
  JetStream rows of the ceiling program (C3–C6, C9) run on the shadow rig; no
  tenant-visible change.
- **Phase B (cutover):** materializer replaces N-row fan-out; functional
  verification beads land here; old path deleted — no permanent dual mode.
  The POC-F4 row-event gate (wamn-lxk) doubles as the cutover regression
  gate: same flow, same assertions, new machinery underneath.
- **Phase C (capabilities):** replay API/UI (+C8); high-rate ingest when a
  driver arrives (MQTT or otherwise); materializer componentization spike
  against #5336 per the adoption plan.

Timing (v2.1): the **core-pivot ladder stays primary**. C7/C2 are safe to
interleave as bounded bench work — they *measure* existing mechanics, they
don't overhaul them; the overhaul (Phases A–C) is strictly sequenced behind
the ladder **and** the Phase 0.5 checkpoint — no parallel paths through the
dispatch machinery the ladder is standing on. A design partner arriving with
fan-out/replay/high-rate-ingest needs pulls A–C forward with this document as
the plan.

## 15. Open decisions this design depends on

| Decision | State | Needed by |
|---|---|---|
| **D19 itself** (JetStream vs retained-events table) | **open — checkpoint after C1/C7/C2** (§10; decision-table row added 2026-07-17) | Phase A go/no-go |
| R6 ordering (`blocking` default) | open | Phase B (and the ladder's 5.11 — decide once) |
| 5.10 payload store backend + event-scoped claim-check retention | open (now prerequisite, scope changed — §4) | Phase B (claim-check) |
| Runtime-DB split | open; C9 informs | independent — composes either way |
| Per-org consumer quota | C4b/C4f | Phase B |
| Retention tiers / pricing | C4d | Phase C / GTM |
| Materializer componentization | adoption plan Phase 4; #5336 spike | post-cutover |

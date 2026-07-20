# Event Plane — CDC (pg_walstream) → JetStream (D19, v3)

> **§1.9a audit (2026-07-19): amendments are additive — base sound.**

**Status:** v3 supersedes v2. Capture is now **CDC via logical decoding**, not
the outbox: events for ANY committed insert/update/delete come from the
database itself (WAL), with filtering purely downstream. Structured as
implementation phases; **Phase 0 blocks all other project work** by owner
decision. Ceilings are measured, not gated (§8); numbers carry provenance
labels (§10).

## 1. Principles

1. **WAL is the event source.** Capture = the commit log itself: atomic,
   total, commit-ordered, source-agnostic (SPA, flow, custom node, psql — all
   just WAL). No opt-in at the capture layer; registration/conditions filter at
   the materializer.
2. **wamn journals intentions and effects-as-facts, never undo.** Replay is
   forward re-execution and re-fires side effects.
3. **Events ≠ execution.** Streams feed the Postgres run queue; they never
   replace it (D3 grounds: write-ahead co-commit, leases, crash-evidence,
   partitions).
4. **Blast radius follows the org** (T2/R8b): streams, NATS accounts,
   retention, replication credentials — all org-scoped.
5. **Ecosystem-standard surfaces** (D17): zero custom WIT in this subsystem;
   guest surface is `wasmcloud:nats` (#5065) when it arrives.

## 2. Architecture

```
any write ─▶ project-env DB ──WAL──▶ CDC reader ──▶ EVT_<org>_<domain> ──▶ materializer ──▶ run queue ─▶ workers
             (app schema +           (native,        (JetStream,            (component        (unchanged)      │
              domain_events)          pg_walstream)   data-plane NATS)       target #5336)                     │
                  ▲                                                                                            │
                  └──────────────── wamn:postgres writes + causation WAL message ──────────────────────────────┘
```

Never on the stream: cron (dispatcher → queue direct), sync webhooks (direct
dispatch, D15), doorbells (control-plane NATS core). Future MQTT ingest
publishes onto the same streams.

## 3. Dropped from the current implementation (the teardown list)

Executed in Phase 2 (§7); listed now so nothing else is built on them:

| Dropped | Replaced by |
|---|---|
| Dispatcher **outbox poller** (poll → N run rows → ack) | CDC reader → stream → materializer |
| **Per-table outbox triggers** + DDL trigger emission (b687d45) | publication `FOR TABLES IN SCHEMA app` (auto-includes new tables) |
| Outbox table + GC for row events | WAL + slot (retention via `max_slot_wal_keep_size`) |
| **R8c** (bulk-write outbox amplification) — *closes* | bulk txns cost nothing extra; decode drains async |
| R9b residual (rename → registration orphaning) — *decode side closed 2026-07-19, wamn-l5i9.11* | relation-OID → catalog-entity mapping at decode; envelopes + subjects key on the stable entity id (rename-proof, gated by the live rename drill). The registration-continuity half lands with the materializer (l5i9.17) |
| N-run-rows-per-event fan-out | one stored event, durable consumer per flow |
| dispatchbench outbox modes | streambench/ceiling benches (§8) |

**Kept, unchanged:** run queue + run state machinery; dispatcher cron path;
write-ahead sync-webhook rows; doorbells; the queue's ordering policies (R6).
**Kept, repurposed:** the *outbox concept* survives as `domain_events` — a
plain table a flow's "emit event" node inserts into within the user's
transaction; CDC captures it like any row. Deliberate application events and
mechanical row changes ride one pipeline, distinguished by subject.

## 4. Capture layer

**Wire schemas — STATUS: FROZEN 0.1.0** (2026-07-19, wamn-l5i9.30). The envelope
`{op, old, new, entity?, table, lsn, txid, commit_ts, causation?}`, the subject
`evt.<org>.<project>.<env>.<entity>.<op>`, the `Nats-Msg-Id =
<project_env>:<lsn>`, the stream name `EVT_<org>_<env>`, the `run_id =
<flow>:evt:<stream_seq padded 20>`, the registration declaration (JMESPath
condition/partition-key over the event context `{op, old, new}`), and the
run-input envelope are frozen into code (`wamn-event-wire`, `wamn-event-reg`,
`wamn-materializer`, `wamn-run-queue`); each is pinned by a golden test — a field
removal/rename breaks a named test. Compatibility rule (the WIT-freeze
discipline): 0.1.x admits only additive or clarifying changes; any breaking
change waits for 0.2.

- **Provisioning** (`provision-project-env` additions): publication over the
  app schema (+ `domain_events`); **failover-enabled slot** (PG18/CNPG — slot
  continuity across switchover is a Phase-1 drill, not an assumption); reader
  registration in the system registry.
  *Shipped (wamn-l5i9.9, 2026-07-18) as the `enable-cdc-project-env` overlay:
  one `wamn_cdc_<org>__<project>__<env>` name for publication (`FOR TABLES IN
  SCHEMA`, auto-includes `domain_events` once it exists) + failover slot
  (SQL-function form, WAL pinned from enable) + REPLICATION role with its own
  `wamn-cdc-…` Secret (the R8b tier named below); `registry.event_readers`
  holds the registration (docs/provisioning.md). Cluster-level knobs
  (`synchronizeLogicalDecoding`, `max_slot_wal_keep_size`) are a provision-org
  sibling bead.*
- **Reader:** dispatcher-family **native** service (posture-doc exception row:
  holds *replication* credentials — a privilege tier above query creds; name it
  in the R8b role scoping). One pg_walstream session per project-env; slot
  admits one consumer, so exclusivity is structural — the dispatcher lease only
  elects *which* replica holds the session; successor resumes from confirmed
  LSN.
  *MVP shipped (wamn-l5i9.10): `wamn-cdc-reader` — one project-env per
  instance, replicas=1 `Recreate` Deployment (event-reader.example.yaml); the
  slot's single-consumer admission is the exclusivity guard and the lease
  election is a filed follow-up (as is fleet/org enumeration). Reads its
  `registry.event_readers` row (never derives names), `StreamingMode::Off`
  (whole txns, commit order, nothing uncommitted leaves the server), LSN
  advance on ack at txn granularity, session re-open loop per the S-CDC-1 F2
  finding. The reader NEVER creates the slot: missing/invalidated ⇒ the §11
  incident, exit loudly (crash-loop = the MVP alert). Envelope/subject/msg-id
  types live in `wamn-event-wire` (FROZEN 0.1.0 at the Phase-2 cutover —
  wamn-l5i9.30; see the §4 status block). MVP entity naming = the pgoutput
  relation's table name —
  the OID→catalog-entity map is the next bead.*
  *Entity keying shipped (wamn-l5i9.11, 2026-07-19): the reader resolves each
  relation OID to its stable catalog entity id via a `wamn_entities` map
  (`relation_oid → entity_id, table_name`), maintained by
  publish/migrate-catalog IN the DDL transaction; OID-keyed, so a rename only
  updates `table_name` (pg_class OIDs survive `ALTER TABLE RENAME`) and
  resolution is timeless under catch-up. The envelope now carries `entity`
  (the id — ABSENT when unmapped, the delayed-never-lost fallback) plus
  `table` (the physical name); the subject's entity segment is the id, so a
  registration's consumer filter is rename-proof (R9b decode side). Resolution
  is lazy per session and never invalidated. Live rename drill + 5 mutants;
  recipe docs/build-and-test.md [EVT-OIDMAP].*
- **Pipeline per event:** typed pgoutput event → OID→entity map → envelope
  `{op, old, new, entity, lsn, txid, commit_ts, causation?}` → subject
  `evt.<org>.<project>.<env>.<entity>.<op>` → publish
  (`Nats-Msg-Id = <project_env>:<lsn>`) → **advance confirmed LSN only on
  JetStream ack**. JetStream down ⇒ LSN holds ⇒ WAL retained (bounded by
  `max_slot_wal_keep_size`, alerted long before) ⇒ delayed, never lost.
  The `Nats-Msg-Id` dedupe is **exactly-once WITHIN the stream's duplicate
  window**; past that window the materializer's `run_id` + `ON CONFLICT` is the
  unbounded guarantee (the window is only the fast path). Because the window's
  size is therefore load-bearing, the reader **asserts** the live stream config
  on start-up and hard-fails on `duplicate_window` / `num_replicas` / `storage`
  drift — it never reconciles a stream it did not create (R12,
  wamn-l5i9.41; decision: REFUSE, matching the never-creates-the-slot posture).
- **Causation (loop-bounding):** when a connection belongs to a run, the
  `wamn:postgres` plugin emits one `pg_logical_emit_message('wamn.causation',
  {run, root, depth})` per transaction; the reader stamps it onto that txn's
  envelopes. Transactional, guest-unforgeable, zero schema footprint. The
  materializer enforces max depth + cycle check; refusals are a distinct,
  alertable outcome. *(pg_walstream surfaces typed protocol Message events —
  confirmed. The **reader-stitch half is done** (wamn-l5i9.12.1): `with_messages`
  + **buffer-per-txn** — the whole txn is held and every row publishes at
  `Commit` with the stamp attached, so causation is robust to whether the
  message frame arrives before or after the rows. Only a **transactional**
  `wamn.causation` frame counts (the unforgeable property rides on the commit).
  The **plugin-emit half is done** (wamn-l5i9.12.2): the trusted flow-runner
  declares the run it drives through a new `wamn:runner/causation.set-run-context`
  channel (additive; `wamn:postgres` stays FROZEN 0.1.0), and the plugin appends
  the transactional emit to `begin_with_claims`, so every run-owned txn is
  stamped `{run, root: run, depth: 0}` (event-chain root/depth thread from the
  materializer, l5i9.17). Guest forgery is blocked: a raw-SQL `wamn.*` emit is
  rejected on the query/execute/cursor surface. MVP: root runs only —
  self-root, depth 0.)*
- **Old images:** `REPLICA IDENTITY FULL` is a **per-entity knob the DDL
  engine manages**, set only where a registration needs old-image conditions
  ("changed-to") — WAL cost is paid per table, not universally. Materializer
  tolerates TOAST unchanged-column markers.
- **Oversize payloads:** claim-check into the payload store (5.10 —
  prerequisite).
- **Library policy:** pg_walstream (BSD-3, v0.8, single-author) is
  **vendored/forked and pinned** like wash-runtime — ledger entry, sync
  discipline; never casually `cargo update`d.

## 5. Stream, materializer, replay (condensed; semantics as v2)

- **Streams:** `EVT_<org>_prod` / `EVT_<org>_dev` on a **dedicated data-plane
  NATS** (JetStream, R3 file; control-plane NATS untouched). `EVT_trials`
  shared, subject-isolated. Per-org accounts. Retention = replay horizon =
  billable tier knob. *Cluster stood up 2026-07-18 (wamn-l5i9.7): 3-node
  JetStream, R3 file storage (deploy/infra/nats-jetstream.yaml, Service `evt-nats`,
  distinct from the untouched control-plane NATS); the `streambench` gate proves
  publish / consume-in-commit-order / `Nats-Msg-Id` dedupe / R3-survives-node-loss
  on the single shared account — per-org accounts are the wamn-4xw seam.*
- **Materializer:** durable consumer per subscribing flow (registration:
  entity id, ops, condition, partition-key expr). Condition evaluates **here**
  (hot-editable; filtered-out events remain in the stream, so condition edits
  are replayable). Deterministic `run_id = <flow>:evt:<stream_seq>` +
  `ON CONFLICT DO NOTHING` = the exactly-once guarantee (dedupe window is only
  the fast path). Delivery order = stream order = **commit order per DB**
  (stronger than the outbox's per-project seq). `partitioned(key)` extracts the
  key from the payload; **R6 `blocking` is load-bearing — decide before
  Phase 2.** ~~v1 native in dispatcher~~ *(superseded by the Service-first
  rework, E11/D21+E12)*. *Shipped 2026-07-19 (wamn-l5i9.17), SERVICE-FIRST: a
  wasi:cli/run Service workload (`spec.service`,
  deploy/platform/materializer.example.yaml) — the first `wamn:jetstream`
  importer (plugin wired into the washlet; the post-commit doorbell rides the
  host's control-plane client, tenant host-derived). Decisions are the pure
  `wamn-materializer` crate: tenant guard (a DELETE under REPLICA IDENTITY
  DEFAULT or a tenant-less table is an alertable refusal, never a cross-tenant
  enqueue), causation depth 16 with the chain THREADED through the run input
  (the flowrunner declares the materializer-minted `{run,root,depth}`, so hop
  N+1's envelopes carry `depth+1` — the budget is real), root-`old` conditions
  HELD until l5i9.31 (old-absent = cannot-evaluate, never condition-false),
  key+policy stamped kq0z-coherently from the flow's fqg.20 declaration (the
  registration's extractor evaluates over the event context), and the E4
  `stream_seq` BIGINT carried on every evt row (run ids zero-padded — the
  belt). Gate: matbench (real guest + real deploy/sql DDL via `include_str!` +
  real JetStream; 17 asserts incl. a server-side-consumer-delete full
  redelivery — 608 collisions, zero new rows); recipe
  docs/build-and-test.md [EVT-MAT]; first C-MAT numbers in docs/ceilings.md.
  One workload per project-env × tenant (v1); replay + EVT-COMPONENT
  (per-org #5336 component) stay downstream.*
- **Replay:** new consumer at past position; replay-namespaced run ids
  (`<flow>:replay:<id>:<seq>`); **re-executes side effects** — opt-in,
  permission-gated, audited.
- **Trigger node UX:** a registration, not code — entity picker, op boxes,
  condition builder off catalog metadata; rename-proof by entity-id keying;
  11.8 impact analysis covers registrations.

## 6. wasmCloud alignment (condensed)

Capture = control-plane native (justified exception; replication sessions are
the shape components are worst at — parser-only wasm build of pg_walstream is a
future decoder-component seam, noted, not planned). Delivery/consumption =
wasmCloud-idiomatic: data-plane NATS split is wasmCloud's own doctrine;
materializer is the platform's best #5336/component candidate (push-shaped, two
WIT imports, stateless, per-org); per-org streams pinned to leaf nodes later is
the Posture-C residency story.

## 7. Implementation phases (this doc's spine)

### Phase 0 — decisions + spikes (blocks everything; target: ~1–2 wks)
**Decisions to sign:** R6 ordering (`blocking` default); 5.10 payload-store
backend (now dual-prerequisite: claim-check + node streaming); envelope/subject/
`Nats-Msg-Id`/`run_id` schemas frozen into code; replica-identity policy;
causation depth default (~8).
*Signed 2026-07-18 (wamn-l5i9.1):* R6 `blocking` carries to the materializer;
5.10 backend deferred to a Phase-1 spike (wamn-l5i9.29); schemas stay working
drafts through Phase 1 and freeze at the Phase-2 cutover (wamn-l5i9.30);
replica identity = the per-entity knob as written (wamn-l5i9.31); causation
depth = **16** (owner override of the proposed ~8).
**Spikes:**
- **S-CDC-1 (pg_walstream diligence):** sustained soak w/ keepalive+feedback
  over idle hours; CNPG switchover with failover slot (resume, no gap);
  TOAST-marker surfacing; 1M-row streamed transaction memory profile;
  Message-event support (causation). Any failure → fallback assessment
  (Supabase `etl`) before proceeding.
  *Done (`wamn-l5i9.2`, 2026-07-18): all five checks pass; findings F1 (crate
  failover-slot syntax bug, worked around; fork patch on `wamn-l5i9.8`) and F2
  (reader session re-open loop, on `wamn-l5i9.10`) recorded.*
- **S-CDC-2 (Sequin calibration, 2–3 days):** stand up Sequin→NATS on one
  staging org against the *same* subject contract — a working reference +
  ceiling calibration + the documented buy-fallback if S-CDC-1 sours.
  *Skipped (`wamn-l5i9.3`, owner decision 2026-07-18): S-CDC-1 did not sour —
  build-vs-buy rests on its results plus Sequin's vendor-published numbers
  (the §10 row stays "unverified locally"); the banked calibration plan is
  preserved in the bead's notes as the buy-fallback starting point.*
**Benches (existing infra only):** C-QUEUE (retire the D3 folklore number) +
C-WAL-0 (baseline WAL volume of representative app load, pre-CDC) — **both
measured 2026-07-18** (`docs/ceilings.md`).
*C-WAL-0 re-sequenced (owner decision 2026-07-18): it was not needed for the
build-vs-buy signature and now gates Phase-1 capture instead — `wamn-l5i9.9`
(publication/slot provisioning) depends on `wamn-l5i9.4`, keeping the baseline
strictly pre-CDC. Done `wamn-l5i9.4` (`docs/ceilings.md` § C-WAL-0): per-op
WAL/op (narrow + wide/TOAST) + representative receiving-event bytes/s.*
**Docs:** teardown list (§3) circulated so no new work lands on the outbox
path; posture rows (reader exception, replication-credential tier).

### Phase 1 — capture in staging (~2–3 wks)
Provisioning (publication/slot/registry); reader MVP (one project-env → real
`EVT_` stream); OID→entity mapping incl. rename drill; causation message in the
plugin + reader stitching; claim-check path.
*Data-plane NATS shipped (wamn-l5i9.7, 2026-07-18): deploy/infra/nats-jetstream.yaml
(3-node R3 JetStream), `streambench` in-cluster gate of record — the substrate
the reader (l5i9.10) publishes onto and C-JS (l5i9.15) benches; left standing.*
*Capture provisioning shipped (wamn-l5i9.9, 2026-07-18): the
`enable-cdc-project-env` overlay — publication + failover slot + replication
role/Secret + `registry.event_readers` registration (§4); proven live on the
wamn-pg pool. The reader MVP (l5i9.10) consumes the registration.*
*Causation reader-stitch shipped (wamn-l5i9.12.1, 2026-07-19): the reader
enables protocol Messages and buffers each txn, stamping a transactional
`wamn.causation` `{run,root,depth}` onto every one of its row envelopes at
`Commit` (robust to frame order); gated live (both orderings + absent + rolled-
back-emits-nothing) + in-cluster on the R3 stream. The plugin-emit half is the
split sibling l5i9.12.2.*
**Benches:** C-CDC (decode drain rate after bulk import; slot-lag knee vs
sustained write rate; WAL delta under FULL identity per table class;
switchover drill timed), C-JS (JetStream bare ceilings: publish/deliver/
consumer-count/storage-per-event/heal time).

### Phase 2 — materialize + cutover (~2–3 wks)
Registration surface (catalog + minimal API; editor panel later); materializer
(~~native v1~~ Service-first per E11/D21+E12) with condition eval, causation
enforcement, `ON CONFLICT` enqueue;
shadow (dual-run vs old path, one week of POC traffic) → cutover → **execute
the §3 teardown** (delete poller, trigger emission, outbox GC; migrate
dispatchbench modes). Functional-verification beads (crash orderings,
redelivery, ordering-under-failure) land here — implementation-time, per owner
instruction not pre-specified as gates.
*Registration surface shipped (wamn-l5i9.16, 2026-07-19). Materializer shipped
(wamn-l5i9.17, 2026-07-19, §5 status note): Service workload + wamn:jetstream
first importer + causation thread + matbench gate; first C-MAT numbers
(deliveries→enqueue + duplicate-storm cost) recorded in docs/ceilings.md
(local provenance — the in-cluster campaign re-measures). Wire schemas FROZEN
0.1.0 into code (wamn-l5i9.30, 2026-07-19, §4 status block). Next on this phase:
l5i9.32 cluster knobs, l5i9.18 shadow/cutover (against the frozen shapes).*
**Benches:** C-MAT (deliveries→enqueue rate, duplicate-storm cost), C-E2E
(commit→run-start distribution; fan-out 1→N vs old path — the one
before/after chart), C-INTERFERENCE (app-CRUD p99 while capture+materialize run
at 80% of knee; one dataset also informing the runtime-DB-split question).

### Phase 3 — capabilities (post-cutover)
Domain-event node (`domain_events` insert); trigger-node editor UX; replay
API/UI (+C-REPLAY drain/interference); per-org consumer quota from C-JS;
retention tiers priced from C-JS storage data; materializer componentization
spike against #5336 (adoption plan); MQTT ingest inherits the rails at the
industrial tranche.

## 8. Ceiling program (measurement, not gates)

Philosophy unchanged from v2: **curves and knees, no pass/fail** — owner
attaches decision rules after numbers exist. Output: `docs/ceilings.md`
capacity model (per org: sustained/burst events/sec, retention GiB/day,
app-path p99 impact), every figure dated + environment + raw-data pointer.
Methodology: fixed reference env; harness ceiling mode (step-ramp to knee =
p99 doubling or lag divergence; 30-min soak at 80%; 10×/60 s bursts); uniform
metrics (stage p50/p99/p999, CPU, WAL bytes/s, bloat, autovacuum share, slot
lag, consumer lag, disk growth). Bench set: C-QUEUE, C-WAL-0, C-CDC, C-JS,
C-MAT, C-E2E, C-INTERFERENCE, C-REPLAY (phased above). C-EVTBL (retained-
events-table knee) runs **only if** the §9 retreat is ever invoked.

## 9. Alternatives on record

- **Sequin (MIT, service):** proven PG→NATS CDC; the buy-fallback and Phase-0
  calibration rig. Costs: Elixir service per org/shared, vendor roadmap,
  overlapping exactly-once machinery. Retreat path if the Rust reader
  underdelivers.
- **Debezium Server:** most mature; JVM + config weight; envelope not ours.
  Documented second fallback.
- **Retained-events table in Postgres:** zero new infra; knee unmeasured
  (C-EVTBL if invoked). Weakened by the CDC decision (capture-everything into a
  table = self-inflicted write amplification CDC avoids); kept for the record.

## 10. Provenance ledger

| Claim | Status |
|---|---|
| Plugin ~2k qps p99<10ms; dispatch p99s | **measured** (p0/queuebench) |
| Queue ~1–5k transitions/s | **measured** (C-QUEUE = wamn-z7b.1, 2026-07-18, `docs/ceilings.md`): untuned 60 s knee ~2000–2500 transitions/s, sustained ~550–1400/s under stock autovacuum; tuning matrix pending (wamn-z7b.6) |
| Baseline app WAL (pre-CDC denominator) | **measured** (C-WAL-0 = wamn-l5i9.4, 2026-07-18, `docs/ceilings.md`): ~200–310 B/op narrow write; ~7 KB / ~14 KB wide-row insert/update (TOAST); ~1.3 KB per typical receiving event. The denominator every C-CDC (wamn-l5i9.14) WAL-delta divides by |
| Sequin ~40–50k ops/s, 55ms | **vendor-published**, unverified locally |
| pg_walstream perf/robustness | **unknown** → S-CDC-1 |
| 256 KiB payload cap; 10-min dedupe window; consumer quota | **proposed knobs** → C-JS/C-CDC inform |
| Causation depth 16 | **decided** (wamn-l5i9.1, 2026-07-18; doc proposed ~8) |
| MQTT "10k+ msg/s bursts" | **industry assumption** |

Rule: no number enters a design doc unlabeled.

## 11. Sharp edges (standing register)

Slot pins WAL → `max_slot_wal_keep_size` + invalidation alert + resync runbook
(a slot invalidation IS a gap — first-class incident); serial decode per
project-env is the new capture ceiling (C-CDC measures; streamed-txn support is
the unbounded-memory answer); failover-slot continuity is verified, not
assumed; single-author library → vendored/pinned/ledgered; replication
credentials = new top privilege tier; stream retains *every* change (bigger
honeypot → per-org accounts mandatory; GDPR answer = bounded retention, in
writing); two-layer exactly-once interleavings verified at Phase 2; second
durability domain on-call (raft, disk, lag, retention).

**Runbook — sustained publish stall (E2):** a held LSN is delivery being
*delayed, never lost* — but it also freezes WAL retention on the source DB, so
an unreachable JetStream silently marches the slot toward invalidation. The
reader escalates to a distinct `CDC_PUBLISH_STALLED` event past
`--stall-threshold-secs` and a slot-headroom monitor alerts before `wal_status`
leaves `reserved` (`slot_safe_wal_bytes`). **On a sustained stall, fix JetStream
— do NOT drop the slot.** Dropping the slot "fixes" the disk by *creating the
gap* it was protecting against; recovery is then re-enable CDC + a backfill
assessment (§4, v3 §11), not a slot drop.

## 12. Open decisions

| Decision | Needed by |
|---|---|
| R6 `blocking` default | ~~Phase 0 sign-off~~ **signed 2026-07-18** (carries to the materializer) |
| 5.10 backend | ~~Phase 0 sign-off~~ **deferred to a Phase-1 spike** (wamn-l5i9.29, signed 2026-07-18) |
| Schemas (envelope/subjects/ids) frozen | ~~Phase 0~~ ~~at the Phase-2 cutover~~ **FROZEN 0.1.0 into code 2026-07-19** (wamn-l5i9.30; §4 status block) |
| Replica-identity policy + causation depth | ~~Phase 0~~ **signed 2026-07-18** (per-entity knob, wamn-l5i9.31; depth 16) |
| Reader build (pg_walstream) vs buy (Sequin) | ~~end of Phase 0 spikes~~ **signed 2026-07-18: build** (`wamn-l5i9.6`, on S-CDC-1 results + vendor numbers; S-CDC-2 skipped — Sequin stays the documented fallback, Debezium second) |
| Consumer quota / retention tiers | Phase 3 (from C-JS) |
| Runtime-DB split (D19-adjacent) | independent; C-INTERFERENCE informs |

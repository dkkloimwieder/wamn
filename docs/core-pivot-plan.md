# Core Pivot Plan

**Date:** 2026-07-15 · **Updated:** 2026-07-18 · **Status:** active ordering (supersedes the "finish the tiering epic first" directive) — **currently suspended by the event-plane v3 Phase 0** (owner decision 2026-07-18, see the event-plane section): Phase 0 of `wamn-l5i9` blocks all other project work; the ladder + tracks resume after Phase 0 unless the owner redirects.

## Why

The four-tier Postgres topology (`wamn-q3n`) landed fully, and an external architecture
review plus our own read agreed we moved into operational **tiering ahead of the product**.
Nothing to unwind — the tiering work is done — so this is purely a **re-ordering** back to
core: prove the platform **executes flows correctly** and exposes a **correct API surface**,
demonstrated by a graduated ladder of live POC flows.

## North-star

- **Correct/proper flow execution + API surface.** Prove it with a *ladder* of live flows,
  trivial → the receiving POC.
- **Not now:** users/auth (4.2/4.3/8.1), all UI, deep security (8.2–8.7), cluster IaC/GitOps (E1).
- **Kept in core:** the control-plane **API** (provisioning saga orchestrator) so standing up
  each POC project is repeatable — *without* the admin-console UI.

## Track 1 — Correct execution (the ladder) · primary

Keystone first — nothing runs live until it exists:

- ~~**`wamn-fqg.8` [P1]** — deploy the live runner~~ **DONE 2026-07-16** (`c40ffef`) — the
  dispatcher → queue → runner chain runs as a live service (`run-worker` + `deploy/runner.yaml`).

Then climb (`wamn-ojm` epic — **auxiliary, capability-gated**; each rung a small *deployed*
flow + execution gate):

1. ~~`wamn-ojm.1` — single-node flow live on the runner~~ **DONE** (`1c60838`)
2. ~~`wamn-ojm.2` — multi-node linear (transform chain)~~ **DONE** (`e5ff9da`)
3. ~~`wamn-ojm.3` — branching logic (conditional + merge)~~ **DONE** (`8145bb7`) — the
   conformance ladder is COMPLETE (`docs/exec-ladder.md`)
4. `wamn-24i` — **POC-F3** async cron escalation — **PARKED 2026-07-16** (dkk): F3 leans on
   three then-unbuilt platform pieces; build them first rather than paper over with caveats:
   - ~~`wamn-17o` [5.9] credential vault~~ **DONE 2026-07-16** (`4ce52a7`,
     `docs/credential-vault.md`) — incl. the fail-closed run-worker egress handler
     (`--allowed-hosts`, empty = deny-all)
   - `wamn-fqg.11` [5.14/2.6] egress governance on the run-worker path — **half-landed**
     with 17o (host-level allowlist); remaining = per-FLOW allowlists (F3's
     `allowedHosts=[notify.example]`) + provisioning-driven entries
   - `wamn-fqg.12` [POC-F3] scale-to-zero / parked-wake proof (P3, deployment topology)
5. `wamn-lxk` — **POC-F4** async row-event + 429 throttle — **reworked to a CDC
   row-event flow** (D19 v3, 2026-07-18; no new work lands on the outbox path),
   dep-gated on the Phase-2 cutover (`wamn-l5i9.18`); 429-throttle scope and the
   cutover-regression role survive unchanged
6. `wamn-1ab` — **POC-F2** custom node ← `wamn-7j0.1` guard → `wamn-bd5` (5.6) → `wamn-0si` (5.5)
7. `wamn-2ft` **POC-DEMO** + `wamn-3rj` **POC-TESTS** — receiving acceptance capstone

Vault follow-up (not F3-blocking): `wamn-fqg.13` [5.9] live K8s Secret credential source
(shares `wamn-5x0.1`'s client).

Engine support pulled in only as a rung needs it: ~~`wamn-1d4`~~ (5.11 ordering
policy — **done**, D20, commit 84233fa; split into `wamn-fqg.18` record-stream
dispatch/D9 + `wamn-fqg.19` cron-misfire/R8d), ~~`wamn-fqg.18`~~ (**done
2026-07-17** — combined claim/checkpoint/complete statements + guest plan cache,
~66 → ~32–37 ms/record; the design pass split out `wamn-fqg.20` flow-level
ordering declaration + dispatcher key-stamping, and bumped `wamn-fqg.9`
guest partitioned claim P3→P2 — those two close the 5.11 surface),
`wamn-dq5` (5.12 cancel), `wamn-sdp` (5.10 payload store).

## Track 2 — API surface correctness · primary, interleave

- `wamn-32n` — 4.4 hot reload (schema change → live API)
- `wamn-tsn` — 4.5 OpenAPI + **GraphQL** SDL + TS SDK (GraphQL currently missing)
- `wamn-2e3` — 4.6 rate limiting / pagination / query-cost
- migration-correctness follow-ups as they surface: `wamn-c6q`, `wamn-6eb`, `wamn-hch`, `wamn-5x0.3`
- *skipped:* 4.2/4.3 auth

## Track 3 — Control-plane API · parallel, in-core

- **`wamn-2ib` [P1]** — 10.1 provisioning **saga orchestrator** only (resumable, compensating
  driver over `provision-org` / `provision-project-env` / `copy-project-env` +
  `provisioning.sagas` + the `q3n.8` saga builders). **Admin console UI deferred.** Its
  cjv.7 quiesce prerequisite is closed by the unified copy (`wamn-8df.5`, 2026-07-17:
  `copy` records a saga per step and cutover refuses until quiesce+verify are recorded);
  remaining prerequisite = cjv.20 registry `validate()` completeness (partly closed by
  8df.3's `validate()` rework — re-check the bead) + the per-step `saga_steps` ledger.

## Support (kept active, not parked)

- `wamn-yf3` — 9.3 production logging (P1)
- `wamn-srb` — 9.6 node-level I/O capture / run history (the n8n-parity feature; sequence once
  the execution ladder matures)
- `wamn-jn6` — 9.8 metric set (also unblocks the deferred `q3n.12`)

## Event-plane program (D19 **decided** 2026-07-18 — v3: CDC → JetStream; Phase 0 blocks everything)

**Owner decision 2026-07-18:** `docs/event-plane-jetstream.md` **v3** supersedes the
v2 outbox-relay candidate (v2 preserved at `docs/event-plane-jetstream-outbox.md`).
Capture is **CDC via logical decoding (pg_walstream)** → JetStream — the WAL is the
event source; the outbox trigger path is retired (v3 §3 teardown: dispatcher outbox
poller, per-table triggers + DDL emission, outbox table + GC, dispatchbench outbox
modes). **No new work lands on the outbox path**; deletion executes at
`wamn-l5i9.19` (Phase 2). Tracker: epic **`wamn-l5i9` [EVENT-PLANE-V3]**, phases 0–3.

- **Phase 0 blocks all other project work** (owner decision 2026-07-18): ~~owner
  sign-offs (`wamn-l5i9.1`)~~ (signed 2026-07-18), ~~pg_walstream diligence spike
  (S-CDC-1, `l5i9.2`)~~ (done 2026-07-18, all five checks pass — `5c3cdf6`),
  ~~Sequin calibration (S-CDC-2, `l5i9.3`)~~ (skipped 2026-07-18, owner decision —
  build-vs-buy rests on S-CDC-1 results + vendor-published numbers; banked plan
  preserved in the bead's notes), C-WAL-0 baseline (`l5i9.4` — re-sequenced
  2026-07-18 to gate Phase-1 capture: `l5i9.9` depends on it, keeping the
  baseline pre-CDC), ~~the docs pass (`l5i9.5`)~~ (done — `ff147f1`),
  ~~build-vs-buy (`wamn-l5i9.6`, owner)~~ (**signed 2026-07-18: build** —
  vendored/pinned pg_walstream; Sequin stays the documented fallback).
  **Phase 0 is complete** — the suspension lifts: the ladder and other tracks
  resume, and epic Phase 1 is unblocked (~~`l5i9.8` vendor/fork~~ done
  2026-07-18 — fork branch `wamn/0.8.0` pinned, ledger
  `docs/pg-walstream-fork.md`; `l5i9.7` ready; `l5i9.9` behind `l5i9.4`).
  Next pick is the owner's.
- Measurement already banked (pre-decision, still load-bearing): ~~C7/C-QUEUE~~
  (`wamn-z7b.1`, `docs/ceilings.md` — untuned knee ~2000–2500 transitions/sec) +
  ~~C2 outbox-trigger overhead~~ (`wamn-z7b.2`, `docs/ceilings.md` § C2 — now a
  historical record of the retired path's cost). Bench renames v2→v3: C1→C-EVTBL
  (contingency-only; prototype parked on `park/c1-eventsbench`), C7→C-QUEUE,
  C3/C5→C-MAT, C4→C-JS, C6→C-E2E, C8→C-REPLAY, C9→C-INTERFERENCE; new C-WAL-0.
  The `z7b.6` tuning matrix is re-parented under the epic (P3).
- 5.10 (`wamn-sdp`) is now an **unconditional** dual prerequisite (claim-check
  payload objects + node streaming); its backend decision lands in `wamn-l5i9.1`.

## Parked (demoted to P3)

- **UI:** 3.3 designer (`wamn-ivi`), 5.8 flow editor (`wamn-8wg`), E6 frontend
  (`wamn-iz5` + children), POC-DM2 (`wamn-srz`), POC-SPA (`wamn-3n3`), admin console
- **Auth / users:** 4.2 (`wamn-0xd`), 4.3 (`wamn-sbh`), 8.1 IdP (`wamn-117`)
- **Deep security:** 8.2 tenant-isolation model (`wamn-5ts`), 8.3–8.7
- **Cluster IaC / GitOps:** E1 (`wamn-bp4`) — `afw` `x09` `6oa` `6s1` `d8i` `pb3`
- **Tiering:** `wamn-q3n` (done; `q3n.12` deferred pending 9.8)

## Suggested first picks

~~`fqg.8` → ladder rungs~~ (done) → ~~`fqg.11`~~ (done, unparks F3 with `fqg.12`) →
~~`1d4` R6 decision~~ (**done** — D20 chosen: `blocking` default, commit 84233fa; the
old `1d4` bead is closed and split into `fqg.18` record-stream/D9 + `fqg.19`
cron-misfire/R8d) → ~~`fqg.18` record-stream dispatch~~ (**done 2026-07-17**;
split out `fqg.20` ordering declaration + key-stamping, `fqg.9` bumped to P2) →
~~`d8v` GC half~~ (**done 2026-07-18** — dispatcher-tick maintenance step +
`outbox_prune_sql`, unblocks `z7b.2`; the amplification half split to
`wamn-vbl`, production janitor wiring filed as `wamn-71t`) →
**event-plane v3 Phase 0 (`wamn-l5i9` — blocks all other work, 2026-07-18)** →
then resume: `POC-F3` / `POC-F4` (F4 now a CDC flow, gated on `l5i9.18`) →
`4.4` hot-reload → (parallel) `2ib`.
Bench days when convenient: ~~`z7b.1` (C7)~~ (**done 2026-07-18**, `docs/ceilings.md`) /
~~`z7b.2` (C2)~~ (**done 2026-07-18**, `docs/ceilings.md` § C2) —
measurement-only, safe to interleave. The D19 decision checkpoint is **retired** —
decided 2026-07-18 by the owner-authored v3 (`z7b.3`/`z7b.4` closed superseded;
the C1 prototype is parked on `park/c1-eventsbench` as the C-EVTBL contingency).

## bd encoding

- **P1** = active pivot: `2ib`, `yf3`, and the active-track epic containers
  (E2/E4/E5/E8/E9/POC). (`fqg.8` closed.)
- **P3** = parked (above). Bump back anytime the plan changes.
- The execution ladder (`wamn-ojm.*`) is P2 and **dependency-gated** behind `fqg.8` so it
  never surfaces as ready before the capability exists.

# Core Pivot Plan

**Date:** 2026-07-15 · **Updated:** 2026-07-16 · **Status:** active ordering (supersedes the "finish the tiering epic first" directive)

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
5. `wamn-lxk` — **POC-F4** async row-event + 429 throttle
6. `wamn-1ab` — **POC-F2** custom node ← `wamn-7j0.1` guard → `wamn-bd5` (5.6) → `wamn-0si` (5.5)
7. `wamn-2ft` **POC-DEMO** + `wamn-3rj` **POC-TESTS** — receiving acceptance capstone

Vault follow-up (not F3-blocking): `wamn-fqg.13` [5.9] live K8s Secret credential source
(shares `wamn-5x0.1`'s client).

Engine support pulled in only as a rung needs it: `wamn-1d4` (5.11 ordering),
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

## Event-plane program (D19 candidate — measurement interleaves, overhaul sequenced)

`docs/event-plane-jetstream.md` (v2.1) + the D19 decision-table row. The ladder stays
primary; **no parallel overhaul of the dispatch machinery the ladder stands on.**

- **Interleave OK (bounded bench work, measures existing mechanics):** C7 queuebench
  ceiling mode + C2 outbox-trigger overhead (C2's GC sub-measure after `wamn-d8v`).
  These retire folklore D3 already depends on — worth it regardless of D19's outcome.
- **After (or alongside late ladder rungs):** C1 retained-events knee → **D19 decision
  checkpoint**. JetStream build-out (Phases A–C) only past the checkpoint or on an
  external driver (design partner fan-out/replay; high-rate ingest — MQTT de-scoped
  2026-07-17, assume HTTP).
- Prerequisites either branch: `wamn-d8v` (outbox GC), R6 decision (`wamn-1d4` — 5.11
  needs it anyway), 5.10 scope change noted on `wamn-sdp`.

## Parked (demoted to P3)

- **UI:** 3.3 designer (`wamn-ivi`), 5.8 flow editor (`wamn-8wg`), E6 frontend
  (`wamn-iz5` + children), POC-DM2 (`wamn-srz`), POC-SPA (`wamn-3n3`), admin console
- **Auth / users:** 4.2 (`wamn-0xd`), 4.3 (`wamn-sbh`), 8.1 IdP (`wamn-117`)
- **Deep security:** 8.2 tenant-isolation model (`wamn-5ts`), 8.3–8.7
- **Cluster IaC / GitOps:** E1 (`wamn-bp4`) — `afw` `x09` `6oa` `6s1` `d8i` `pb3`
- **Tiering:** `wamn-q3n` (done; `q3n.12` deferred pending 9.8)

## Suggested first picks

~~`fqg.8` → ladder rungs~~ (done) → **`fqg.11`** (unparks F3 with `fqg.12`) →
**`1d4` R6 decision** (decide `blocking` now, while it's a decision not a rework — F4 is
its first consumer, and it's load-bearing for the event plane too) → **`d8v` GC half**
(small janitor-colocated pruner; F4's live outbox traffic needs it, and it unblocks
`z7b.2`) → `POC-F3` / `POC-F4` → `4.4` hot-reload → (parallel) `2ib`.
Bench days when convenient: `z7b.1` (C7) / `z7b.2` (C2) — measurement-only, safe to
interleave. C1 + the D19 checkpoint (`z7b.3`/`z7b.4`) after F4.

## bd encoding

- **P1** = active pivot: `2ib`, `yf3`, and the active-track epic containers
  (E2/E4/E5/E8/E9/POC). (`fqg.8` closed.)
- **P3** = parked (above). Bump back anytime the plan changes.
- The execution ladder (`wamn-ojm.*`) is P2 and **dependency-gated** behind `fqg.8` so it
  never surfaces as ready before the capability exists.

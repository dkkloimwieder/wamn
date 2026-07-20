# Core Pivot Plan

> **§1.9a audit (2026-07-19): amendments contradict the base — rewrite scheduled (findings §1.9b).**

**Date:** 2026-07-15 · **Updated:** 2026-07-18 · **Status:** active ordering (supersedes the "finish the tiering epic first" directive) — **currently suspended by the event-plane v3 Phase 0** (owner decision 2026-07-18, see the event-plane section): Phase 0 of `wamn-l5i9` blocks all other project work; the ladder + tracks resume after Phase 0 unless the owner redirects. **Cross-cutting sequencing overlay:** `docs/findings.md` §6 (the findings wave/cluster plan; beads: epic `wamn-2jkm` + `wamn-l5i9.39–.58`). **Wave 1 executed 2026-07-19** (`d41e682`…`627a108`: reader hardening R11/E2/R12 · R13 · E13 both halves closed (build denylist + fork TcpConnect deny, rev pin `8b76869`) · E10 `wamn:jetstream@0.1.0` · E11 → **D21** · §1.9a audit · §5.2 reviews → R24–R33/E15–E17 minted, Q1–Q3 closed on evidence). **Wave 2 executed 2026-07-19** (`35a8bff` E1 pipelining · `709d2cf` E4 `stream_seq` · `cebd722` R14 `held_since` · `7b4671f` SR11 `wamn-sql` · `79e414b` R8b-b · `c705c9e` SR12b · `f7652c6` R2/R16 bound `set_config` · `e235abb` R16b `identifiers.rs` · `d770302` R18 · `7f91e3a` SR4 split). **Wave 2.5 executed 2026-07-19** (owner-inserted before SR9): `a59619d` R32 retry→park both drivers (`wamn-2jkm.50`) · fork `eef76cd8` + pin `4e82c8f` E15/E16 UDP arms (`wamn-7j0.2` — **5 carried commits, PAST the escalation threshold: engage upstream**) · `0d560b6` R27 slug injectivity (`wamn-2jkm.45`) · `91659ff` E17 tenant positive allowlist (`wamn-2jkm.52`, unblocked `wamn-bd5`) · `0d7231f` SR12a headers (`wamn-2jkm.17`). **SR9 executed 2026-07-19** (`d4fe3aa`…`685a7fc`, solo main-loop per the owner's "take sr9 on resume"): `wamn-host` split by deployment artifact — `wamn-ctl` (nine verbs, subcommand surface unchanged) / `wamn-dispatcher` (no runtime linked) / `wamn-run-worker` / `wamn-cdc-reader`; six Dockerfile targets; manifests swapped; washlet strings-clean; in-cluster rollout rides `wamn-2jkm.41`. **Materializer-gates wave executed 2026-07-19** (owner-authorized "fqg.20 ∥ l5i9.16 … ratify E8 … fold E7's remainder in"): E8 ratified as **D22** (`055dfe6`, `l5i9.46` closed) · E7 remainder (`f044b5f`, `l5i9.48` closed — zero-grant reader ServiceAccount + credential scope) · `wamn-fqg.20` flow-level ordering + dispatcher key stamping (`c32ffaf`) ∥ `wamn-l5i9.16` EVT-REG registration catalog + minimal API (`b456409`); 94 workspace suites green. Next: the single six-image rebake sweep `wamn-2jkm.41`, then the materializer `wamn-l5i9.17` as a `Service` — ALL its gates are now closed (owner's pick). **2jkm.41 sweep + cleanup wave (kq0z ∥ v6o1 ∥ o3u6/.56) executed 2026-07-19** (`e77008d`…`9fe4a58`; D23 fork-maintainer + D24 refuse-orphaning-publish recorded). **Materializer executed 2026-07-19 (`wamn-l5i9.17`, solo main-loop per the owner's banked directive):** Service-first workload (`spec.service`, deploy/platform/materializer.example.yaml) — the **first `wamn:jetstream` importer** (plugin + post-commit doorbell wired into the washlet; `WAMN_EVT_NATS_URL` in values-wamn.yaml) — over the new pure `wamn-materializer` decision crate (tenant guard, causation depth 16 **threaded through the run input** so chains accumulate, root-`old` conditions HELD until `l5i9.31`, kq0z-coherent key+policy, E4 `stream_seq` adopted into the pure queue model + zero-padded `mint_evt_run_id`); gate = `matbench` (real guest + real deploy/sql DDL + real JetStream: 17 asserts, full-redelivery exactly-once, doorbell rings observed) + 6 mutants killed; first C-MAT numbers in docs/ceilings.md. **Wave 3 executed 2026-07-20 (the owner's banked five-lane wave `.30 ∥ rmxa ∥ .57 ∥ .32 ∥ fqg.9`, five worktree agents + serial main-loop integration):** `ff110d8` `l5i9.30` wire schemas FROZEN 0.1.0 into code (golden drift guards; streambench/matbench/readerbench stand-ins migrated) · `6bd0c08` `rmxa` D24 registration-orphan refusal in publish/migrate-catalog (live gate + mutant) · `2b8728c` `l5i9.32` CDC cluster knobs rendered (always-on `max_slot_wal_keep_size=1GB`; failover-slot-sync on `instances >= 2`; live wamn-pg retrofit = one owner-run `kubectl patch`, blocked by the session permission gate — command in the bead notes) · `89ffce3` `l5i9.57` E10 e2e + `js-sample` adopter template (first `producer` importer; `samplebench` 17 asserts) · `091e35a` `fqg.9` guest-side partitioned claim (5.11 ordering story complete end-to-end; 3 mutants). 97 workspace suites green (new baseline — rmxa's live gate added a binary). **`l5i9.18` shadow/cutover executed 2026-07-20 (solo main-loop, owner's "resume with l5i9.18"):** the comparison DEFINED (v3 §7 Phase-2 status note — compare-only by construction) + the per-flow cutover switch: registration `state: shadow | live` (additive 0.1.x; absent = live) — shadow = full decision pipeline into the new `wamn_run.evt_shadow` ledger (PK-deduped, no run/queue/doorbell), live = materializer fires AND the dispatcher YIELDS the flow from outbox matching (`cdc_live_flows_sql` in the poll txn — mutual exclusion replaces the id-collision safety the v2→v3 run-id change removed). Gate = `cutbench` (ONE write program → real triggers+dispatcher ∥ real slot+reader+materializer.wasm; 26 asserts: bijection on (flow,table,op,row-id), `jsonb_populate_record` payload canonicalization, declared divergence classes CONDITION-SCOPE / EXPECTED-DELETE-RI, flip-window writes exactly-once, cross-path join empty) + matbench/dispatchbench regressions + 4 mutants killed; 97 workspace suites green; fixture postgres gains `wal_level=logical`. Flip runbook in v3 §11; scaffold removal folded into `l5i9.19`; sustained-traffic fence deferred as `wamn-0ynt` (P4). **POC-F4 (`wamn-lxk`) is now UNBLOCKED** (still gated on `bd5` → `1ab`). **BANKED (owner directive 2026-07-20): the next wave is `l5i9.22 ∥ l5i9.31 ∥ wamn-bd5`** — three worktree lanes (the 3-agent cap exactly; opus subagents; no findings.md/board edits from worktrees; doc/board sweep + per-bead commits in the serial main-loop integration pass): **`l5i9.22` [EVT-C-E2E]** commit→run-start distribution + the fan-out 1→1/5/20 before/after chart vs the old N-row path at identical load, plus the 10× burst (lag depth, drain time, app-path impact) — MUST land before the `l5i9.19` teardown (needs the old path alive); ceiling-program methodology, provenance-labelled rows into docs/ceilings.md; **`l5i9.31` [EVT-REPLICA-IDENT]** the per-entity `REPLICA IDENTITY FULL` DDL knob reconciled from registrations (l5i9.1 decision d; NON-RETROACTIVE — document the flip boundary per the Q2/l5i9.56 answer), which unholds the materializer's old-value conditions and retires the EXPECTED-DELETE-RI divergence class (delete-subscribed flows become cut-over-able); **`wamn-bd5` [5.6]** the production runner↔custom-node invocation path (v0 = in-cluster HTTP via the serve-node pattern; the cjv.3 per-run credential-grant primitive is shipped; heed cjv.28's `wamn_nodes::node()` tightening before executing untrusted nodes) — restarts the POC critical path `bd5 → 1ab → lxk(F4) → 3rj/2ft`. After the wave: `l5i9.19` teardown (solo main-loop — big deletion across dispatcher/run-queue/ddl, includes the l5i9.18 shadow scaffolding per its notes). **Wave 4 in flight (2026-07-20, the banked three lanes as parallel worktree agents):** `6adc0da` **`l5i9.31` EVT-REPLICA-IDENT SHIPPED** — pure reconciler in `wamn-migrate` (FULL iff a registration reads root `old` or subscribes to `delete`; cross-tenant union; idempotent d↔f flips, never clobbers `n`/`i`), superuser verb `wamn-ctl reconcile-replica-identity` (D24-style homing), ONE root-`old` detector in `wamn-event-reg` shared by reconciler + materializer, materializer old-conditions UNHELD behind the per-event guard (`old-image-absent` alertable refusal — never condition-false), delete tenant-scoping under FULL, cutbench phase 3 retires EXPECTED-DELETE-RI post-flip while the DEFAULT pin stands; live gate proves the non-retroactive WAL boundary via test_decoding; main-tree re-gate 98 suites + cutbench + matbench green; 3 mutants killed. `15b83a5` **`l5i9.22` EVT-C-E2E code SHIPPED** — `e2ebench` (cutbench-substrate: one writer, real dispatcher old arm ∥ embedded reader→JetStream→materializer.wasm new arm, run-id-namespace attribution): commit→run-start distribution, fan-out 1→1/5/20, 10× burst; local shape (debug, provenance-labelled): old p50 ~27–30 ms vs new ~129–153 ms commit→run-start (a tightly-polled outbox wins steady-state first-enqueue), BUT the app-txn is FLAT in N on both paths — **the "N-outbox-rows-in-the-app-txn" premise was WRONG: the shipped trigger writes ONE row per write; fan-out was always post-commit at the dispatcher** — CDC's case is the trigger-tax removal + app decoupling + off-txn drain (burst: 9 msgs/0.1 s vs 84 rows/0.9 s depth); new-path fan-out cost is the materializer's serial per-registration fetch → `wamn-l5i9.64` (P3). 2 mutants killed. **The in-cluster campaign of record (release image) is the remaining .22 obligation — rides the wave-end rebake sweep; the bead stays open until its rows land in docs/ceilings.md.** `55347b5` **`wamn-bd5` [5.6] code SHIPPED** — production runner↔custom-node invocation: pure `wamn-node-invoke` crate (envelope + per-step grant derivation + 9b config memoization, linked by BOTH ends), flowrunner `custom` dispatch arm POSTs over `wasi:http` (zero new runner WIT — rides the existing egress path), `wamn-host serve-node` runs the node under the REAL frozen `wamn:node` world (grant installed GET-ONLY, project pinned host-side, E17 import screen at load — a node cannot self-grant or cross projects), C2-3 `wamn_nodes::node()` tightened to descriptor-only; live `nodeinvoke` gate green in the main tree (delivery/grant/not-granted×2/memoized-once), 100 workspace suites (new baseline), 3 mutants killed. Trust gap named: runner↔node authn = `wamn-fqg.22` (P2); other deferrals `wamn-fqg.21/.23–.28`, plus pre-existing `runnerbench` stand-in DDL drift found = `wamn-nhjg` (P2). **The POC critical path restarts: `bd5 → 1ab → lxk(F4)`.** **WAVE 4 COMPLETE + SWEEP EXECUTED 2026-07-20** — all three beads CLOSED. In-cluster sweep (one four-image rebake host/gates/ctl/run-worker + kind loads): cutbench-with-phase-3 PASS · matbench PASS · **e2ebench CAMPAIGN OF RECORD PASS** (docs/ceilings.md §C-E2E: trigger tax ~0.25–0.3 ms/write constant and flat in N — the "N-outbox-rows-in-the-app-txn" premise was wrong; outbox wins first-enqueue ~26 ms vs ~157–185 ms p50 at the recorded pacing knobs; CDC wins decoupling + burst absorption — 10× spike, zero sampled consumer lag; release fan-out slope nearly flat → `l5i9.64` softened) · serve-node deployed via deploy/platform/serve-node.yaml (supersedes the S4 spike deploy) + the cross-pod hop proven with a real envelope · hostgroup/runner rolled (**`wamn-rfaz` CLOSED** — the fqg.9+bd5 flowrunner is deployed and claim-quiet). Record-run bench defects found+fixed (`a6dba69`): static provenance labels, observed-peak burst assert (fast-drain artifact), warmup-residue count leak. INCIDENT healed (with the owner): the fixture postgres pod is EPHEMERAL — the l5i9.18 `wal_level=logical` restart wiped its provisioned schemas (runner + dispatcher warn-looped; NOT a guest regression); full restore executed + the proven three-manifestation runbook lives in `wamn-1wdq` (runner demo = run-state+run-queue+flows sed'd; f1 = f1-provision Job + run-queue sed'd); both services verified quiet. **BANKED (owner directive 2026-07-20): the next wave is `l5i9.61 ∥ wamn-fqg.22 ∥ wamn-nhjg`** — three opus worktree lanes off the current tip, serial main-loop integration, same discipline as wave 4 (own target dirs; no board/findings edits from worktrees; per-bead close+commit+push; deferrals filed as beads): **`l5i9.61` [EVT-RI-ORCH]** wire `reconcile-replica-identity` into publish/migrate-catalog + decide the registration-change trigger (lives in `wamn-ctl`/`wamn-migrate`; the verb + pure planner shipped in `6adc0da`; until closed the runbook step is manual); **`wamn-fqg.22` [5.6-AUTHN]** runner↔node authn — build the **signed envelope** (per-project-env HMAC key via the existing runner-credentials Secret pattern; serve-node verifies before installing the grant; flowrunner signs) — mTLS stays the later infra upgrade, NOT this bead; **`wamn-nhjg` [GATE-DRIFT]** align runnerbench's stand-in DDL with deploy/sql/run-queue.sql (partition_owner/partition_policy/stream_seq et al) + a 9mg8-style drift guard, then prove runnerbench green vs the current fqg.9 guest. Excluded deliberately: `wamn-v8cv` (needs the owner's wedge-semantics decision first) and `l5i9.19` teardown (solo main-loop, must not run concurrently with these lanes — ask at the next boundary). **Wave 5 in flight (2026-07-20, the banked three lanes as parallel worktree agents):** `9aa25a7` **`wamn-nhjg` GATE-DRIFT CLOSED** — runnerbench's stand-in `run_queue` DDL predated fqg.9 (missing `partition_policy` + the D20 CHECK, the `run_queue_partition` partial index, and the whole `partition_owner` lease table), so every post-fqg.9 drain tail would have 42P01'd; the stand-in is now byte-aligned with the nodeinvoke/failoverbench siblings and pinned by a drift-guard test that `include_str!`s `deploy/sql/run-queue.sql` and asserts column parity (the 9mg8 pattern; 2 mutants killed by name); runnerbench re-proven green end-to-end in the main tree vs the current fqg.9 guest (drain/reuse/empty/runaway/stream 200/200/stream-reload all PASS — the empty-drain phase IS the fall-through proof: its `run_next` executes both partition statements cleanly). Deferral: a dedicated partitioned-dispatch runnerbench phase = `wamn-7hja` (P4; failoverbench already covers keyed behavior). `93ed048` **`l5i9.61` EVT-RI-ORCH CLOSED** — publish-catalog/migrate-catalog now run the RI reconcile automatically after apply (both verbs already connect as the superuser the ALTER needs; the pass is idempotent, reuses the tested l5i9.31 `reconcile()` path, is scoped strictly to the verb's `--schema` — the cross-tenant union only decides *which registrations* demand FULL — and both verbs gain a `--skip-reconcile-replica-identity` escape hatch); the manual runbook step retires for the publish/migrate path (docs/provisioning.md updated). New pure detect surface `ReplicaIdentityPlan::pending_old_image_gap`. Live gate `ri_orch_live` (flip d→f with bystander untouched · idempotent re-publish · skip honored · plain re-publish resets f→d · migrate-path flip after its apply tx) PASS in the main tree; 3 mutants killed by name. Registration-change API-path hop deferred = `wamn-l5i9.65` (P2; correctness protected by the materializer's alertable old-absent refusal — operational-latency gap only) + response-surface warning = `wamn-l5i9.66` (P3). Rider fix `0a9c2ea`: `wamn-cdc-reader` failed to build standalone (async-nats `jetstream` feature reached it only via workspace feature unification) — feature now declared. `405bd26` **`wamn-fqg.22` 5.6-AUTHN CLOSED — WAVE 5 COMPLETE** — runner↔node authn = the banked signed envelope: HMAC-SHA256 over the exact request-body bytes in `x-wamn-signature`, canonical `sign_envelope`/`verify_envelope` in the pure `wamn-node-invoke` crate (constant-time compare), the per-project-env key = reserved vault credential `wamn:node-invoke-signing-key` in the `wamn-runner-credentials` Secret — the flowrunner reads it via the existing `wamn:node/credentials.get` (ZERO new WIT) and signs; serve-node verifies BEFORE installing the grant (witnessed by a `grant_install_count` assert) and defensively strips the reserved name from any node grant; unkeyed project-env = legacy network-trust with a loud startup warning. Local gates: 12 envelope units + nodeinvoke 17 asserts (AUTHN-POSITIVE/UNSIGNED/TAMPERED/WRONG-KEY/NO-ORACLE/VERIFY-BEFORE-GRANT/SIGNED) + 101-suite workspace + 3 mutants killed by name. IN-CLUSTER GATE OF RECORD: host/run-worker/gates rebaked + kind-loaded, real `openssl rand -hex 32` key patched into the deployed Secret (mirrored under `default` + `demo-project` — the demo manifests disagree on vault project scope = `wamn-gdii` P4), serve-node logs "authn ENABLED", both deployments rolled, unsigned POST → 401 `missing-signature`, openssl-HMAC-signed POST → 200 with the node's contract response (the independent HMAC agreeing with the Rust verifier cross-validates the canonical bytes), runner claim-quiet. Replay-within-env = documented accepted risk. Deferrals: `wamn-fqg.29` (P3 401-terminal on the runner hop) `.30` (P3 rotation accept-both) `.31` (P3 fail-closed toggle) `.32` (P4 replay freshness). mTLS remains the later infra upgrade, per the bank. **BANKED (owner directive 2026-07-20): next is `l5i9.19` [EVT-TEARDOWN], SOLO MAIN-LOOP on resume** — the bead is claimed; its dep (`l5i9.18`) is closed and the C-E2E measure-first ordering is satisfied by the campaign of record. Scope (the bead description + notes are the spec): delete the dispatcher outbox poller, per-table outbox trigger + DDL trigger emission (`b687d45` path, `crates/wamn-ddl` outbox plan), outbox table + GC maintenance (d8v), N-row fan-out; migrate dispatchbench outbox modes → streambench; PLUS the l5i9.18 cutover scaffolding (`wamn_run.evt_shadow`, `shadow_observe_sql` + the guest shadow branch, the dispatcher `cdc_live_flows` yield guard, cutbench's shadow-comparison asserts). **Owner decision MADE 2026-07-20: registration `state: shadow` is REMOVED ENTIRELY** (no permanent dual mode; a future dry-run stage would be designed fresh). Deployed-env care: live provisioned schemas carry outbox tables/triggers — needs the `wamn-1wdq`-class migration/runbook story; the fixture pod is ephemeral. Not pre-authorized beyond `.19`: `wamn-v8cv` (wedge-semantics decision — present options when asked), the cleanup pool (`l5i9.65`, `fqg.29–.32`, `7hja`, `gdii`, …). **`l5i9.19` EVT-TEARDOWN EXECUTED 2026-07-20 (`f0cebca`, solo main-loop per the banked directive):** the D19 v3 §3 teardown is COMPLETE — deleted the dispatcher outbox poller + GC maintenance step + row-event registry halves (the dispatcher is now cron + parked-wake only), the wamn-run-queue outbox module + outbox/shadow/cdc-yield SQL builders, wamn-ddl trigger emission (`Migration::outbox_triggers` / `OutboxOptions` / the emit-outbox example), the `outbox` + `evt_shadow` blocks in deploy/sql/run-queue.sql, registration `state: shadow|live` REMOVED ENTIRELY (the owner decision — a legacy stored `state` key fails parse → HELD, pinned by a named test), the materializer guest's shadow branch, and the outboxbench/cutbench/e2ebench gates + Jobs (the C2/C-E2E campaign records stand final in docs/ceilings.md). dispatchbench REWORKED to cron/ordering/race/fairness/wake/live (ordering re-proven on the cron path; fairness on a due parked-run backlog). Gates of record: 101 workspace suites green; live dispatchbench/matbench/run-queue/ddl/registration vs throwaways; 3 mutants killed by name; IN-CLUSTER dispatchbench-job + matbench-job PASS on the rebaked wamn-dispatcher/wamn-gates images; the dispatcher rolled + sweep-quiet; the live outbox/evt_shadow tables dropped from poc_f1 + wamn_runner_demo (no triggers ever existed live; runbook merged into `wamn-1wdq`); the teardown materializer.wasm pushed to the in-cluster registry. **R8c CLOSED on this commit** (findings board; `wamn-2jkm.31`); `wamn-0ynt` closed moot. New beads: `wamn-3glr` (P3 reader-inclusive RI-flip e2e — the ex-cutbench-phase-3 coverage), `wamn-4768` (P4 CDC-only commit→run-start bench rebuild). **The event-plane Phase-2 core is DONE** (capture → materialize → cutover → teardown); remaining l5i9 opens are hardening/deferral children. Next pick is the owner's — `wamn-v8cv` (wedge semantics, needs the owner decision), the POC chain (`1ab` → `lxk`/F4, now on a clean single-path event plane), or the cleanup pool. **BANKED (owner directive 2026-07-20 'take your recommended parallel plan on resume'): the CLEANUP WAVE — three opus worktree lanes off `baa1eea`, serial WITHIN each lane, serial main-loop integration (per-bead close+commit+push; no board/findings edits from worktrees; own target dirs; disjoint throwaway ports; deferrals filed as beads):** **Lane A [5.6-AUTHN]** `fqg.29` (401→terminal on the runner hop) → `fqg.31` (fail-closed toggle) → `fqg.30` (dual-key rotation window) → `fqg.32` (replay freshness) + `gdii` (vault project-scope manifest mismatch) — all share serve_node.rs/wamn-node-invoke/flowrunner/the nodeinvoke gate, hence one lane. **Lane B [EVT-RI-ORCH]** `l5i9.65` → `l5i9.66` — **owner decision MADE 2026-07-20: the reconcile hop is a PERIODIC CRONJOB** running the existing `wamn-ctl reconcile-replica-identity` verb per project-env (~5 min cadence; no new service, no queue; rejected: dispatcher-drained queue — R8b escalation — and detect-only); then the api-gateway `pending_old_image_gap` response warning. **Lane C [GATE-COVERAGE]** `3glr` (reader-inclusive RI-flip e2e, matbench extension embedding `run_with_token`, own wal_level=logical throwaway) → `7hja` (runnerbench partitioned-dispatch phase). Excluded deliberately: `wamn-4768` (speculative until a re-measure is wanted) and `wamn-v8cv` (needs the owner's wedge-semantics decision — not cleanup). In-cluster work (rebake host/gates/run-worker as needed, serve-node/runner rolls, the .65 CronJob apply, jobs) happens ONCE in the serial main-loop sweep.

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
  dispatcher → queue → runner chain runs as a live service (`run-worker` + `deploy/platform/runner.yaml`).

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
ordering declaration + dispatcher key-stamping (**done 2026-07-19**, `c32ffaf`),
and bumped `wamn-fqg.9` guest partitioned claim P3→P2 — ~~`fqg.9`~~ **done
2026-07-20** (`091e35a`, guest run-next claims partitioned heads in order;
5.11 is now fully closed),
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
v2 outbox-relay candidate (v2 preserved at `docs/archive/event-plane-v2-outbox.md`).
Capture is **CDC via logical decoding (pg_walstream)** → JetStream — the WAL is the
event source; the outbox trigger path is retired (v3 §3 teardown: dispatcher outbox
poller, per-table triggers + DDL emission, outbox table + GC, dispatchbench outbox
modes). **Teardown EXECUTED 2026-07-20 (`wamn-l5i9.19`)** — the outbox path and
the l5i9.18 shadow scaffolding are deleted; row events have one path.
Tracker: epic **`wamn-l5i9` [EVENT-PLANE-V3]**, phases 0–3.

- **Phase 0 blocks all other project work** (owner decision 2026-07-18): ~~owner
  sign-offs (`wamn-l5i9.1`)~~ (signed 2026-07-18), ~~pg_walstream diligence spike
  (S-CDC-1, `l5i9.2`)~~ (done 2026-07-18, all five checks pass — `5c3cdf6`),
  ~~Sequin calibration (S-CDC-2, `l5i9.3`)~~ (skipped 2026-07-18, owner decision —
  build-vs-buy rests on S-CDC-1 results + vendor-published numbers; banked plan
  preserved in the bead's notes), ~~C-WAL-0 baseline (`l5i9.4`)~~ (done
  2026-07-18, `docs/ceilings.md` § C-WAL-0 — still gates Phase-1: `l5i9.9`
  depends on it), ~~the docs pass (`l5i9.5`)~~ (done — `ff147f1`),
  ~~build-vs-buy (`wamn-l5i9.6`, owner)~~ (**signed 2026-07-18: build** —
  vendored/pinned pg_walstream; Sequin stays the documented fallback).
  **Phase 0 is complete** — the suspension lifts: the ladder and other tracks
  resume, and epic Phase 1 is unblocked (~~`l5i9.8` vendor/fork~~ done
  2026-07-18 — fork branch `wamn/0.8.0` pinned, ledger
  `docs/pg-walstream-fork.md`; ~~`l5i9.7` EVT-NATS~~ done 2026-07-18 — the
  data-plane NATS is stood up (3-node R3 JetStream, `deploy/infra/nats-jetstream.yaml`,
  streambench gate) and left standing; unblocks `l5i9.15` [C-JS];
  ~~`l5i9.9` EVT-PROVISION~~ **done 2026-07-18** — the `enable-cdc-project-env`
  overlay: publication + failover slot + replication role/Secret (R8b tier) +
  `registry.event_readers` registration, proven live on wamn-pg
  (`docs/provisioning.md`); unblocks `l5i9.10` [reader MVP]; the cluster-level
  logical-decoding knobs are a filed sibling bead;
  ~~`l5i9.10` EVT-READER~~ **done 2026-07-19** — the reader MVP:
  `wamn-cdc-reader` (one project-env, replicas=1;
  `deploy/platform/event-reader.example.yaml`) + the `wamn-event-wire` draft contract;
  commit-order envelopes onto the R3 `EVT_` stream, LSN advance only on ack
  (JetStream down = delayed-never-lost, proven), missing/invalidated slot =
  the §11 incident; gated live local + in-cluster (readerbench;
  `docs/build-and-test.md` [EVT-READER]); lease election + fleet enumeration
  are filed follow-ups).
  ~~`l5i9.11` EVT-OIDMAP~~ **done 2026-07-19** — relation-OID → catalog
  entity-id keying: the reader resolves each OID via the `wamn_entities` map
  (maintained by publish/migrate-catalog in the DDL txn; OID-keyed, so a
  rename only moves `table_name`), envelopes carry `entity`+`table` and the
  subject keys on the stable id — **the R9b decode side closes** (rename-proof
  subjects; the registration-continuity half rides the materializer `l5i9.17`).
  Live rename drill + 5 mutants; recipe `docs/build-and-test.md` [EVT-OIDMAP].
  ~~`l5i9.12` EVT-CAUSATION~~ **done 2026-07-19** — SPLIT (issues-are-granular)
  into `.12.1` reader-stitch + `.12.2` plugin-emit, both now closed (umbrella
  `.12` closed). `.12.1`: the reader enables protocol Messages and **buffers
  each txn**, stamping a transactional `wamn.causation` {run,root,depth} onto
  every row envelope at `Commit` (robust to frame order; only a transactional
  frame counts — unforgeable); gated live + 3 mutants + in-cluster R3; recipe
  [EVT-CAUSATION-STITCH]. `.12.2`: the trusted flow-runner declares the run it
  drives through a NEW additive `wamn:runner/causation.set-run-context` channel
  (owner unfroze/extended the WIT the guest-driven way — `wamn:postgres` stays
  FROZEN 0.1.0, no S2 re-gate), and the plugin appends the transactional emit to
  `begin_with_claims`, stamping every run-owned txn `{run, root: run, depth: 0}`
  (MVP root runs; event-chain root/depth thread from the materializer `.17`);
  guest raw-SQL `wamn.*` emit blocked. Gated: unit (emit bytes + batch wiring +
  forgery guard) + live runnerbench + a test_decoding decode probe (the real
  plugin emit rides each run's sink txn, content == run_id) + 3 mutants; recipe
  [EVT-CAUSATION-EMIT].
  Phase-1 remaining: `l5i9.14` [C-CDC] + `l5i9.15` [C-JS] ready;
  `l5i9.32` [EVT-CLUSTER-CONFIG] blocks `l5i9.18`. Next pick is the owner's.
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

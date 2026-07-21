# wamn — Findings Ledger

**The single findings document.** Absorbs `review-findings.md` (R1–R9c) and
`structure-review.md` (SR1–SR7) from the repo, and **mints R10–R16, SR8–SR14,
and E1–E14 here** (from the 2026-07-18 review passes; none of those IDs existed
in the prior files). The prior ledgers fragmented the same question —
*what is open, how bad, what next* — across three files, three numbering
schemes, and three sequencing sections, requiring cross-references to be
readable. That was the problem, not the fix.

**Identifiers are preserved** (R/SR/E prefixes) so existing beads, commits, and
conversations keep resolving. They are now *sections of one ledger*, not
separate documents.

**Status rule (adopted, see R10):** *A finding closes on a commit that removes
or fixes code — never on a decision that plans to. Decisions change a finding's
**priority**; only commits change its **status**. Questions close on verified
evidence, cited to source.* Every `closed` row below carries its commit, bead,
or evidence citation.

**Sources merged:** internal architectural passes (2026-07-11 … 07-18, tips
`155ac4b` → `8f1b53d`), the pinned-fork audit (`dkkloimwieder/wasmCloud`
`d3d83f3`; `pg-walstream` `wamn/0.8.0`), and a second external static-read pass
(2026-07-18, tip `8f1b53d`).

---

## 0 — Status board

Priority is (impact ÷ cost), not severity. **§1 comes first**: it is the
prerequisite that makes everything else findable.

| # | Finding | Sev | Status | Do when |
|---|---|---|---|---|
| **§1** | **Docs consolidation + archive (single source of truth)** | — | **closed** | `b7fa9af`…`6ac07d9` (2026-07-19, wamn-2jkm.1–.6); residuals as beads: §1.5=wamn-2jkm.28, §1.9a=wamn-2jkm.10, in-cluster deploy verify=wamn-2jkm.41 |
| SR14 | D4/D19 contradiction unmarked in the decision table (§1.2) | High | **closed** | `b7fa9af` (wamn-2jkm.1; table sweep found no other same-shape row) |
| §1.9a | Amendment-density audit (verdict per file) | Med | **closed** | `3a3bb34` (wamn-2jkm.10; 15 stamped — 13 additive, 2 contradict → rewrites wamn-2jkm.59/.60; platform-plan re-audit wamn-2jkm.63) |
| R10 | R8c closed against code that ships; adopt the closure rule | High | **closed** | `c6f2f54` (wamn-2jkm.5; rule in AGENTS/CLAUDE; audit: 8 closures PASS, R8c reopened=wamn-2jkm.31, SR5 corrected the other way) |
| R13 | `next_interval` panics on `min > max` (unvalidated CLI) | Med | **closed** | `6b22e84` (wamn-2jkm.7; `Cadence::new → Result`, silent `.max()` coercion removed) |
| R11 | Reader reopen: no backoff, no cap, budget reset on *open* | High | **closed** | `d41e682` (wamn-l5i9.39; one ladder both arms, productivity reset, rate cap) |
| E2 | Reader stall: no alarm, no attempt metric, no slot headroom gauge | High | **closed** | `7147f07` (wamn-l5i9.40; `CDC_PUBLISH_STALLED` + slot-headroom monitor; exporter follow-up wamn-2jkm.54) |
| E13 | `wasi:sockets` unconditional; `TcpConnect` ignores `allowedHosts` | **Crit** | **closed** | build `845d023` (wamn-2jkm.8) + fork `8b76869`/pin `627a108` (wamn-7j0.1); UDP arms → E15 (wamn-7j0.2); runtime gate rides wamn-2jkm.41 |
| E4 | `run_id` lexical vs numeric `stream_seq` | High | **closed** | `709d2cf` (wamn-l5i9.43; `stream_seq BIGINT` ahead of `run_id` in every claim key; zero-pad mint + pure-model field ride l5i9.17) |
| E1 | Sequential publish caps capture at ~1/RTT | High | **closed** | `35a8bff` (wamn-l5i9.42; acks settle at Commit, in-flight 256, retry from first unacked; readerbench rides wamn-2jkm.41) |
| E10 | `wasmcloud:messaging@0.2.0` cannot carry the materializer (verified) | High | **closed** | `f8f7abd` (wamn-l5i9.44; `wamn:jetstream@0.1.0` + host plugin); e2e rider delivered `89ffce3` (wamn-l5i9.57 — js-sample first producer importer, samplebench 17/17 in-cluster vs evt-nats) |
| E11 | Native-service drift; adopt the default rule | High | **closed** | `cb86099` (wamn-l5i9.45; **D21**) |
| E12 | `Service` workloads exist in 2.5.2 — corrects E11's run-worker verdict | High | **closed** | recorded in D21 (`cb86099`); implementation = wamn-l5i9.17 then .49/.50 |
| SR11 | Positional SQL params compose across crates with no type | High | **closed** | `7b4671f` (wamn-2jkm.19; `wamn-sql` leaf — `Sql{text,arity}.param()`; three call sites renumber against head arity) |
| R16 | R2 propagated (`app.runner`); duplicated, diverged validators | Med | **closed** | `f7652c6` + `e235abb` (wamn-5x0.2/wamn-2jkm.20; all four claims bound; one validator owner `identifiers.rs`) |
| R2 | Claim interpolation → `set_config` binds | Med | **closed** | `f7652c6` (wamn-5x0.2; bound `CLAIM_SQL`, template deleted; S2 re-gate rides wamn-2jkm.41) |
| R12 | Stream config drift: `get_or_create_stream` never asserts | High→Med | **closed** | `e350524` (wamn-l5i9.41; REFUSE posture; E1 unblocked) |
| R14 | Held outbox rows head-of-line-block the poll window | Med | **closed** | `cebd722` (wamn-2jkm.18; `held_since` exclusion + backlog age; dispatchbench scenario rides wamn-2jkm.41) |
| R1 | Park/wake consumes the redelivery budget | High | **closed** | `9de70c2` (wamn-fqg.5) |
| R3 | Per-component memory limits | Med-High | **closed** | `c3356ea` (wamn-bp4.1) + fork ResourceLimiter commit |
| R4 | Fork-based upstream management | — | **closed** | `dd0d60d` (wamn-bp4.2) |
| R6 | `partitioned(key)` ordering under retry/park | High | **closed** | `84233fa` (policy materialized on the row; D20 is the decision, not the evidence) |
| R8c | Outbox amplification + GC | Med | **closed** | `f0cebca` (wamn-l5i9.19 teardown removed the subject — the outbox + its GC are deleted; wamn-2jkm.31) |
| SR1/SR3/SR6 | Gates split, repo tiering, conventions written down | — | **closed** | `3dfee03` / `4a637e2` / `d8e1366` |
| E14 | Q1: `ev.lsn` is per-message — dedupe design sound | — | **closed** | evidence: `pg-walstream stream.rs:1093,1066` (question-class closure); standing guard shipped `7440d33` (wamn-l5i9.55) |
| SR12 | Pure/effect split can't test statement-level bugs | High | **closed** | live-test half `c705c9e` (wamn-2jkm.23); header qualification + composed-statement convention `0d7231f` (wamn-2jkm.17) |
| SR9 | `wamn-host` is three programs in one crate | Med | **closed** | `d4fe3aa`+`7262679`+`157b61b`+`685a7fc` (wamn-2jkm.22) — wamn-ctl / wamn-dispatcher / wamn-run-worker / wamn-cdc-reader split; washlet strings-clean; in-cluster rollout rides wamn-2jkm.41 |
| E7/E8 | Reader as a service: extraction + placement/ownership | Med/High | **closed** | E7 `f044b5f` (wamn-l5i9.48; extraction = SR9 `d4fe3aa`/`157b61b`/`685a7fc`, remainder = zero-grant ServiceAccount + credential scope; in-cluster apply rides wamn-2jkm.41) · E8 = **D22** `055dfe6` (wamn-l5i9.46 ratified; lease-sharded fleet, per-org escape hatch; `.33`/`.34` implement) |
| SR8 | `deploy/` 68 flat files — canonical: §1.6 | — | **closed** | `8123046`…`6ac07d9` (wamn-2jkm.6; local gates only — in-cluster run rides wamn-2jkm.41) |
| SR13 | Two sources of truth for schema | Med | open | next platform-schema change |
| SR4 | `wamn_postgres.rs` split (grew 18% since filing) | Med | **closed** | `7f91e3a` (wamn-cjv.18; `{mod,types,pool,claims,resources}.rs`; claims.rs = the claim boundary as one unit) |
| SR10 | `wamn-gates` flat at 18.8k lines | Med | open | next bench |
| SR2 | flowrunner re-implements run-state SQL | Med | open | before F3/F4 |
| R17 | `NAMEDATALEN` truncation: `wamn_mig_drop_` + long entity collides; `TempNameCollision` compares untruncated | Med | **closed** | `df9fc38` (wamn-2jkm.30) — aside derivation truncates + stable hash-suffix at 63 bytes, `TempNameCollision` compares PG-visible names; the schema-wide truncation-aware catalog guard landed alongside (`dbe0026`, C1-2/wamn-cjv.9) |
| R18 | `standard_conforming_strings` assumed, never asserted | Med | **closed** | `d770302` (wamn-2jkm.21; `post_create` SHOW assert per physical connection, fail closed) |
| R19 | `row_to_map` lossy on non-UTF-8 (`from_utf8_lossy`) | Low | **closed** | `1f21432` (wamn-2jkm.35) — fallible `row_to_map`, non-UTF-8 refuses `Config`→`Fatal` (loud exit, slot holds WAL) instead of `U+FFFD` corruption |
| R20 | Author-supplied retry `cap-ms` unbounded | Low | **closed** | `225dfec` (wamn-2jkm.36) — parse-time clamp to `CAP_MS_CEILING` 1 h (the janitor reap grace); default untouched |
| R21 | `classify` matches `Display` text; PG17+ floor unstated | Low | open | with reader work |
| R22 | `subject_token` collisions (`a.b` ≡ `a_b`) | Low | **closed** | `fa79b79` (wamn-2jkm.38) — stable FNV-1a hash-suffix when sanitization changed the string; clean tokens byte-identical (freeze held); E3 (cross-schema) stays open, decoupled — post-freeze it is a 0.2 wire change; residual: wamn-iaq9 |
| R23 | Unbounded `OFFSET` in the API gateway | Low | open | with keyset pagination |
| R24 | Merge/loop flows unresumable (occurrence collapse) | Med | **closed** | `6edd545` (wamn-03m+cjv.10+2jkm.42) — engine-computed per-visit occurrence end-to-end (builders bind `$3`, never literal 0); visit-by-visit replay; legacy collapsed history fails LOUD (Mismatch); standing guard = runnerbench merge-resume phase (park-mid-merge reconstruction, 7 per-visit rows); in-cluster failoverbench/ladderproof re-run PASS on the rolled runner |
| R25 | `idempotency_key` collides across visits | Low | **closed** | `6b525e7` (wamn-2jkm.43) — `run:node:occurrence`; retries keep their key, distinct visits differ, resumed visits reconstruct the same key |
| R26 | `resume` folds error-routes as Success (`step_seq`/`result` drift) | Low | **closed** | `4918df7` (wamn-2jkm.44) — replay routes ERROR_PORT records via the helper shared with live `error_or_fail`: step_seq/result untouched, occurrence still advances |
| R27 | Slug `--` separator not injective — cross-tenant name collision on the shared pool | High | **closed** | `0d560b6` (wamn-2jkm.45) — both validators + SQL CHECK reject `--` runs; injectivity test; live PG gate |
| R28 | CDC replication credential blast radius is cluster-wide, not "one registration" | Med | open | wamn-2jkm.46, with the l5i9.32 knobs |
| R29 | Replication-slot shape never reconciled (R12 class) | Low | open | wamn-2jkm.47 |
| R30 | Vault secrets plaintext-resident, no zeroization | Low | open | wamn-2jkm.48 |
| R31 | Plugin claim/grant registries never cleared on unbind | Low | **closed** | `f072590`+`fa96675` (wamn-2jkm.49) — `on_workload_unbind` reaps both plugins' per-component registries (fork builtin convention); serve-node per-invocation grant revoked by Drop-guard; nodeinvoke GRANT-REVOKED witness |
| R32 | `Retryable` node errors abort the invocation and hold the lease (prod dispatch path) | High | **closed** | `a59619d` (wamn-2jkm.50) — Step::Wait → park in BOTH drivers, attempt persists across reclaim; in-cluster verify rides wamn-2jkm.41 |
| R33 | Delay wake key is global per run — second delay never delays | Low | **closed** | `579bb05` (wamn-2jkm.51, 2026-07-20 cleanup wave) — wake keyed by node id, cleared on emit; legacy bare wake = ignore-and-clear (row swept late: the fix predates this sweep) |
| E15/E16 | UDP egress allow-all; `UdpBind` all-interfaces (the arms E13's fix left) | High/Med | **closed** | fork `eef76cd8`/pin `4e82c8f` (wamn-7j0.2) — same opt-in as E13, UdpBind = TcpBind posture; runtime gate rides wamn-2jkm.41; **5 carried commits = past the fork escalation threshold** |
| E17 | `egressbench` would PASS a tenant `wamn:postgres` importer | Med (latent) | **closed** | `91659ff` (wamn-2jkm.52) — tenant positive allowlist single-sourced in `egress_guard`; unblocks wamn-bd5; builder interface lint = wamn-2jkm.68 |
| R8b-b | Tenant predicate on the four RLS-only queue statements | Low | **closed** | `79e414b` (wamn-2jkm.53; four builders carry the predicate; R8b-a stays wamn-286) |
| Q1 (§5.1) | `--features caps` not tenant-reachable; `wamn-1nd` stays future conditioning | — | **closed** | evidence in §5.1 (wamn-2jkm.15; minted E17) |
| Q2 (§5.1) | REPLICA IDENTITY de-facto contract = DEFAULT, key-only; `l5i9.31` is non-retroactive | — | **closed** | evidence in §5.1 (wamn-l5i9.56; design para → l5i9.17) |
| Q3 (§5.1) | No `wamn_dispatch` role exists; `wamn_app` verified `NOBYPASSRLS` non-owner FORCE-RLS, live | — | **closed** | evidence in §5.1 (wamn-2jkm.16; R8b split → wamn-286 / wamn-2jkm.53) |
| R5, R7, R9a–c, R15, E3, E5, SR7 | see sections below | Low–Med | open | opportunistic (E6 closed `9ea8da0`; E9 closed `db4d891`; R9a closed — shipped `30be826`, coverage verified wamn-2jkm.32) |

**Deferred by owner decision:** CI/LICENSE (§5.4 records the evidence-based
re-open argument, unactioned); TRUNCATE handling (E5 — the prior question is
undecided, see §5.3).

---

## 1 — Reorganization (do first)

The single source of truth is currently 39 docs, no index, with the entry path
in the root README failing on its first hop and a decision table that
contradicts itself. Everything else in this ledger is harder to action until
this is fixed.

### 1.1 `docs/README.md` — the index that does not exist

`docs/` is 39 `.md` files / ~735 KB / ~10,840 lines with **no index** (the
full directory incl. schemas, WIT, and ceiling CSVs is 65 files / ~796 KB; an
earlier draft's "868 KB" was an `ls` block-count artifact). The root README
says *"start with `docs/platform-plan.md` and the decision table"* — and there
is no file or heading called "the decision table"; it is a section titled
**"Decision Boundaries & Alternatives (denoted)"**. For a repo whose stated
principle is AI-legibility, whose `AGENTS.md`/`CLAUDE.md` point agents at
`docs/` as authoritative, the single documented entry path does not resolve.

**Write `docs/README.md`** with four sections, in this order: **Start here**
(platform-plan → decision table anchor → core-pivot-plan → this ledger);
**Current by subsystem** (the table in §1.4); **Results & measurements**
(p0-results, ceilings — with their provenance caveats named); **Archive**
(what moved and why). Link the decision-table *anchor*, not the file.

### 1.2 (SR14) D4 vs D19 — the table contradicts itself, unmarked

`platform-plan.md:200` carries D4 (*outbox + dispatcher poller; LISTEN/NOTIFY
removed entirely;* **Locked for correctness**; *CDC is the scale-up path*) and
`:215` carries D19 (*CDC via logical decoding → JetStream; retires the outbox
trigger path entirely;* **Decided**). Same table, same subject, opposite
answers, neither row referencing the other — and D4 sorts first, is still
marked **Locked**, and still lists CDC as its *rejected alternative*. Anyone
resolving "how does this platform capture DB events" from the decision table
gets the retired answer.

**Fix:** one line in D4's status cell — `**Superseded by D19** (2026-07-18)`.
Cheapest high-value edit in the repo. Then sweep the table for the same shape
(any row whose alternative column names something a later row adopted).

### 1.3 Archive: what moves, what stays

Convention: superseded material moves to **`docs/archive/`** with a version in
the filename, keeping a one-line pointer at the top of the archived file
(`superseded by <current> on <date> — retained for <reason>`).

| File | Verdict | Reason |
|---|---|---|
| `event-plane-jetstream-outbox.md` (442 ln) | **archive** → `archive/event-plane-v2-outbox.md` | v2, superseded by v3. Today the dead doc is *larger, more specific, and sorts first* — filename, size, and `ls` order all select it. Retain: the outbox-era rationale and the teardown list's provenance |
| `p0-exit-criteria.md` (46 ln) | **archive** → `archive/p0-exit-criteria.md` | P0 closed; results live in `p0-results.md`. Retain: the go/no-go thresholds that gate re-measurement |
| `poc-material-receiving.md` (73 ln) | **keep** | still the acceptance spec for P1/P2 and reference solution #1 |
| `poc-f1.md`, `poc-dm1.md` | **keep** | shipped POC slices, current |
| `p0-results.md` (707 ln), `ceilings.md` (334 ln) | **keep**, banner | measurement records. **Add the `fsync=off` banner** (E6): shape-only, not citable externally |
| `review-findings.md`, `structure-review.md` | **archive** → `archive/` | absorbed by this ledger; keep for commit-message resolution |
| `core-pivot-plan.md` | **keep** | live status ledger, correctly marked suspended by event-plane Phase 0 |
| `build-and-test.md` (1,643 ln) | **keep**, restructure | see §1.5 |
| everything else (subsystem docs-of-record) | **keep** | one per subsystem, current, well-named |

**No other file is superseded.** In-place amendment density is unaudited —
supersession/amendment language appears in ~20 of the 39 files
(`deployment-model`, `provisioning`, `postgres-topology`, `schema-lifecycle`,
`registry-model` lead), which is probably legitimate in-place amendment but has
not been checked file-by-file. The subsystem docs remain the corpus's real
strength — each a doc-of-record with a predictable name. The problem was never
volume; it was the absence of an index and two unmarked supersessions.

### 1.4 Subsystem doc map (for `docs/README.md`)

Catalog/schema: `catalog-model`, `app-schema`, `schema-lifecycle`,
`ddl-compiler`, `migration-engine`, `rls-builder`, `seed-data`.
Execution: `flow-schema`, `flow-runner`, `node-library`, `exec-ladder`,
`run-queue`, `run-state`, `wamn-node-design-notes`, `wamn-node.wit`.
Data path: `security-db-path`, `wamn-postgres.wit`, `credential-vault`.
Event plane: `event-plane-jetstream` (v3, current), `pg-walstream-fork`.
Platform/infra: `platform-plan`, `deployment-model`, `postgres-topology`,
`system-cluster`, `registry-model`, `provisioning`, `wasmcloud-utilization`,
`wash-runtime-fork`, `api-gateway`, `tracing`.
POC: `poc-material-receiving`, `poc-f1`, `poc-dm1`.
Process: `core-pivot-plan`, `findings.md` (this), `build-and-test`,
`p0-results`, `ceilings`.

### 1.5 `build-and-test.md`: 96 KB, two headings, keyed by bead id

1,643 lines with exactly **two** `##` headings ("Build environment", "Gates by
bead") and 57 `###` under the second — indexed by **bead id**, the most
perishable identifier in the system (meaningless once a ticket closes), and it
is the *only* place gate invocations exist. **Fix:** re-key by
crate/subsystem, one section per gate family, bead ids demoted to a
cross-reference column. Split per subsystem if it stays unwieldy.

### 1.6 (SR8) `deploy/` — 68 flat files, five lifecycles

Install-once infra (`cnpg-*`, `nats-jetstream`, `loki*`, `tempo*`, `minio`,
`otel*`, `barman-cloud-plugin`, `kind-config`, `values-wamn`) · production
manifests (`dispatcher`, `runner`, `registry`, `wamn-sysdb`,
`api-gateway-workload`, `event-reader.example`, `trace-relay-workload`,
`*-credentials.example`) · ~25 gate Jobs (`*-job.yaml`) · POC assets
(`f1-*`, `poc-material-receiving.*`, `proof-catalog.json`) · raw SQL
(`app/catalog/system-schema`, `run-queue`, `run-state`, `flows`,
`postgres-init`).

**Fix (pure `git mv`, five batches for readable history):**
`deploy/{infra,platform,gates,poc,sql}/`, `cred/` unchanged. Then grep-fix
`Dockerfile`, `AGENTS.md`/`CLAUDE.md` snippets, `build-and-test.md`, and the
Jobs' own volume paths; add `deploy/README.md` naming what belongs in each
tier (the rule that stops it re-flattening). **Verification:** a full
in-cluster gate run from the moved paths.

### 1.7 Code: what is obsolete vs what needs reorganizing

**Nothing is dead code today.** The distinction that matters:

**(a) Scheduled for deletion, still live — do not treat as gone (R10).** The
outbox capture path: `wamn-ddl/src/outbox.rs`, `wamn-run-queue/src/outbox.rs`,
`outbox_{poll,ack,insert,prune}_sql`, ~40 references in
`wamn-host/src/dispatch.rs`, `outboxbench`, `ddl/examples/emit-outbox.rs`,
plus references in `wamn-api`, `wamn-flow`, `wamn-catalog`, `wamn-provision`
(26 files). D19 Phase 2 deletes it **after** the materializer ships and the
cutover passes. Until then it is the **only working capture path in
production**, and R14 is a live liveness bug in it.

**(b) Needs reorganization, not deletion:**
- `wamn-host` (10,015 ln / 24 files) — three programs in one crate: the
  washlet (`engine`, `host`, `plugins/`), ten one-shot control-plane verbs
  (`provision*`, `dump/restore/copy_project_env`, `migrate_catalog`,
  `publish_catalog`, `enable_cdc_project_env`, `env_policies`), and three
  long-lived services (`dispatch`, `run_worker`, `event_reader`). → **SR9**,
  and E12 changes the destination for two of the three. *(Done — SR9 closed
  2026-07-19, `d4fe3aa`+`7262679`+`157b61b`+`685a7fc`: split into `wamn-ctl` /
  `wamn-dispatcher` / `wamn-run-worker` / `wamn-cdc-reader`; `wamn-host` is the
  washlet only.)*
- `wamn-gates` (18,803 ln / 29 flat modules, 27.8% of all Rust) → **SR10**.
- `wamn_postgres.rs` (1,788 ln, **+18% since SR4 was filed**) → **SR4**.
- `flowrunner` re-implements run-state SQL that `wamn-run-store` owns → **SR2**.

**(c) Genuinely fine, leave alone:** the 22-crate pure/effect split for
everything except the two above; `components/` tiering (`fixtures/`,
`samples/`, `poc-` prefix) — SR3 shipped and *held*, which is evidence the SR6
conventions write-down works; `poc/` as a top tier; the fork ledgers.

### 1.8 Adopt: the closure rule and one index per ledger

The status rule at the head of this document, plus: **this ledger is the only
findings file.** Reviews produce sections, not documents. **Growth rule** (so
§1.5 is never written about this file): every finding with a proposed fix gets
an ID and its own board row — no catch-all rows minting untracked prose (the
R17–R23 correction is the precedent); observations without an action live in §5
notes, un-IDed; grouped board rows are permitted only for **listed** IDs that
each have a section anchor; when a finding closes, its full narrative moves to
`docs/archive/findings-closed.md` leaving the board row + a one-line summary;
if any single section exceeds ~150 lines it is a sign the finding is really a
design doc — write the doc, leave a pointer. Bound: when the open board exceeds
~40 rows, the oldest opportunistic tier gets a scheduling decision or an
explicit `wont-fix`, not silence.

### 1.9 Ongoing docs policy: audit, amend-vs-rewrite, consolidate

§1.3 ruled on the four files that are superseded *today*; this section is the
policy for the other 35 over time — the piece the feedback's item 7 exposed as
missing (the "nothing else is stale" claim was backed by an audit of two
files).

**(a) Amendment-density audit — a tracked work item, not a claim.**
Supersession/amendment language appears in ~20 of the 39 docs. Audit
file-by-file, starting with the five densest: `deployment-model` (8 hits),
`provisioning` (7), `postgres-topology` (6), `schema-lifecycle` (5),
`registry-model` (5). Each file gets one of two verdicts, recorded at its top:
**amendments are additive, base is sound** (no action) or **amendments
contradict the base** (schedule a rewrite). Until audited, a doc's currency is
*unknown*, not presumed.

**(b) The amend-vs-rewrite rule.** In-place amendment is fine while additive.
The moment an amendment *contradicts* base text rather than extending it, the
doc gets rewritten to say what is true now, and the prior version moves to
`docs/archive/<name>-vN.md`. Amendment stacking is exactly how the D4/D19
contradiction happened (§1.2) — a Locked decision amended around instead of
superseded in place. Contradiction, not age or size, is the trigger.

**(c) Archive, never delete.** Commit messages, beads, and this ledger cite
docs by path; deletion breaks resolution, archiving does not. `docs/archive/`
entries keep a one-line header: superseded by what, when, retained for what.

**(d) Consolidation candidates come out of the audit, not intuition.** Likely
merges the audit should confirm or refute: `run-state.md` → `run-queue.md`
(one subsystem, two files); `seed-data.md` → `schema-lifecycle.md`;
`poc-dm1.md`/`poc-f1.md` → sections of `poc-material-receiving.md` once the
POC epic closes. Asserting these now without the audit would repeat the
"nothing else is stale" mistake — the audit produces the verdicts, this
ledger tracks them.

---

## 2 — Correctness (R-series)

*Full narratives for R1–R9c are preserved from the prior ledger; condensed here
to problem → fix → status. Closed items retain their evidence for commit
resolution.*

### R10 — R8c was closed against code that ships *(High, process)*
`review-findings.md` recorded R8c as *"Closed — D19 v3 retires the outbox
capture path; amplification is moot."* The outbox ships in 26 files and is the
only working capture path (§1.7a); the materializer that replaces it is not
built. A scale finding was closed on the grounds that its subject does not
exist. **This closure was mine.** The structural cause: `docs/` is the source
of truth, `docs/` describes the intended future, so findings close against
systems that have not shipped.
**Fix:** reopen R8c (or reclassify *deferred pending D19 Phase 2*) with the
deletion beads named; adopt the closure rule (head of this doc + AGENTS.md);
audit the other 2026-07-18 closures against it.

### R11 — Reader reopen loop: no backoff, no cap *(High, liveness)*
`event_reader.rs`: the **open** path (`:340-348`) counts failures, bails at 10,
sleeps 2 s. The **drain** path (`:370-374`) does `reopens += 1; warn!` and falls
straight back round the loop — no sleep, no cap. `:351` resets
`consecutive_failures` on *open* success, before `drain` runs. A session that
opens cleanly and severs immediately hot-loops `preflight` → connect → sever as
fast as Postgres answers, and the cap can never trip. `Protocol` is reachable
from ordinary code (`:459`) and falls to `_ => Reopen`.
**Fix:** one backoff/cap ladder shared by both arms; `drain` returns
`DrainSummary { commits }` and the counter resets only when `commits > 0`
(measure *productivity*, not open success); cap reopens **per unit time** so a
slow flap is caught too. **Verify:** stubbed stream that opens-then-errs
terminates within the cap; live walsender that accepts-and-drops shows bounded
attempts and a nonzero exit. Add the contract to the module header's
load-bearing list.

### R12 — Stream config drift *(High until the materializer ships, then Med)*
`get_or_create_stream` (`:310`) never reconciles; the CLI help says plainly
that an existing stream keeps its config — so `--dup-window-secs` (120) and
`--stream-replicas` (3) are **inert** against a pre-existing `EVT_` stream,
including one silently at R1.
**Framing (corrected per feedback 2026-07-19):** an earlier draft downgraded
this to Medium on the grounds that the materializer's `run_id` + `ON CONFLICT`
is the plane's real guarantee — **while noting the materializer does not
exist**. Rating a live gap against an unshipped absorber is R10 with the sign
flipped: don't *close* against the future, and don't *downgrade* against it
either. **High until the absorber ships**, Med after; and E1 *depends* on this
finding (E1 widens the crash-republish exposure from 1 in-flight message to
~256, and its recovery argument leans on exactly the dedupe-window and
`ON CONFLICT` properties this finding says are unverified). **R12 lands before
E1.**
**Fix:** read back `StreamInfo` after get-or-create and hard-fail on
`duplicate_window` / `num_replicas` / `storage` mismatch, reporting both
values; decide and record whether the reader may `update_stream` or must
**refuse** (refusing matches the "the reader NEVER creates the slot" posture);
amend the `:20` module doc and `event-plane-jetstream.md` §4 to say
"exactly-once *within the duplicate window*, with `ON CONFLICT` as the
unbounded guarantee."

### R13 — `next_interval` panics on `min > max` *(Med, production panic)*
`run-queue/src/dispatch.rs`: `current.saturating_mul(2).clamp(min, max)` —
`Ord::clamp` panics when `min > max`; the args are unvalidated CLI/env
(`dispatch.rs:111,116`; `run_worker.rs:127,132`). `--min-interval-ms 5000
--max-interval-ms 1000` starts cleanly, serves traffic, and panics on the first
**idle** sweep — it survives every smoke test that has work to do.
**Fix:** per M-PANIC-ON-BUG this is user input, not a broken invariant —
`bail!` at config construction naming both values; better, `Cadence::new(min,
max) -> Result<_,_>` so the check happens once at the boundary
(M-STRONG-TYPES-GUARD).

### R14 — Held outbox rows block the poll window *(Med, liveness — live per R10)*
`outbox_poll_sql` is `WHERE dispatched_at IS NULL ORDER BY seq … LIMIT n`;
`plan_ack` correctly refuses to ack **held** rows (an active-but-unparseable
flow), so they stay `NULL` forever and, being oldest, permanently occupy the
lowest `seq` slots. Once `--batch` (64) held rows accumulate, row-event
dispatch **stops project-wide for every flow** because of one broken flow.
**Fix:** (A, no schema change) pass the held `(table, event)` set into the poll
and exclude it — the dispatcher already reads the registry inside the
transaction, so reorder rather than add a round trip; (B, preferred) a
`held_since timestamptz` the poll filters on, which also gives the backlog an
**age** to alert on. Either way bound the backlog and escalate past it.
**Verify:** pure-layer poll-window model; `dispatchbench` with an invalid flow
generating `batch + 1` events plus one healthy event.

### R15 — Wake scan pins the cadence at min behind a wedged partition *(Med)*
`parked_due_sql` lacks the `partition_key IS NULL` guard both claim paths carry,
so partitioned followers enter `report.woken` every sweep, `found_work()` is
true, and `next_interval` returns `min`. Behind a D20 `blocking` wedge this is
permanent (the janitor is exempt from reaping a blocking head by design): one
wedged key holds the whole project's dispatcher at the 250 ms floor and defeats
"zero continuous polling."
**Fix:** add the guard, or exclude partitioned wakes from `found_work()` —
cadence must reflect *actionable* work.

### R16 — R2 propagated; validators duplicated and diverged *(Med)*
`wamn_postgres.rs:749` still interpolates the claim preamble; since R2 was filed
a **third** interpolated claim landed (`:772`, `app.runner`) with a **third**
validator (`:350`). R2's gate was *"before the `format!` pattern propagates."*
It propagated. `valid_tenant` now exists twice with different rules
(`wamn_postgres.rs:329` bounds length at 64; `dispatch.rs:143` does not).
**Fix:** land R2's `set_config(name, value, is_local := true)` rewrite
**extended to `app.runner`** (one more column in the same bound `SELECT` —
which is the argument for now rather than after a fourth claim); one crate owns
the identity-format validators, both sides import them; keep the grep-gate (no
`format!` containing `SET LOCAL` in the plugin). **Verify:** R2's gate set plus
a test that plugin and dispatcher agree on a 65-character tenant.

### R1–R9c (prior pass) — status
**R1** park/wake budget *(closed, `wamn-fqg.5`)* · **R2** claim interpolation
*(open — see R16)* · **R3** per-component memory limits *(closed, fork commit
#2)* · **R4** fork-based upstream management *(closed)* · **R5a/b** RLS
claim-shape `NULLIF` + S2 scope honesty *(open, low)* · **R6** partition
ordering policy *(closed, D20)* · **R7a/b** failover status-flip alerting +
two-lease latency *(open, low)* · **R8a** cron anchor vs retention *(open —
decide before 9.6 retention)* · **R8b** dispatcher DB role scoping *(split 2026-07-19 by the Q3 answer —
no `wamn_dispatch` role has ever existed; the dispatcher runs as `wamn_app`,
verified `NOBYPASSRLS` non-owner under FORCE RLS, statically and live.
**R8b-a** narrow dedicated role stays open = wamn-286; **R8b-b** explicit
tenant predicate on the four RLS-only statements = wamn-2jkm.53)* ·
**R8c** outbox amplification/GC *(**closed** at `f0cebca` — the l5i9.19 teardown removed the subject; wamn-2jkm.31)* · **R8d** cron misfire
collapse *(open, doc)* · **R9a** reserve the `wamn_` identifier prefix at
catalog validation *(**closed** — already shipped at `30be826` (wamn-66x);
coverage over all four author-identifier classes verified complete and
pinned, wamn-2jkm.32 — synthesized/enum/relation/seed identifiers confirmed
non-gaps)* · **R9b** rename × row-event registration
*(closed at `wamn-l5i9.17` — the decode half landed at l5i9.11 (OID→entity-id
keying, rename drill live) and the registration-continuity half now rides the
materializer's id-keyed match: `catalog.event_registrations.entity_id` ==
`envelope.entity`, both the STABLE catalog id, so a table rename orphans
nothing (matbench + the `wamn-materializer` entity-guard units); the
schema-qualifier residual is E3 = `wamn-l5i9.52`)* · **R9c** one-transaction apply
expiry (`CREATE INDEX CONCURRENTLY`) *(open, doc)*.

### R17–R23 — lower-severity, each with an ID and a board row
**R17 (Med)** `NAMEDATALEN` bound on derived identifiers — and the path is
worse than "collisions after truncation": `wamn_mig_drop_` + a ~50-char entity
name truncates at 63 bytes, the aside-rename is followed by a `DROP`, and
`TempNameCollision` compares **untruncated** names — so the collision the
check exists for is exactly the one it cannot see. Fix at the identifier-derivation
seam (length-check + hash-suffix), with the migration engine's next touch. ·
**R18 (Med)** quoting assumes `standard_conforming_strings = on` — assert via
`SHOW` at connect; **do with R16/R2** (same file, same injection surface; the
only cheap moment is while that file is open). ·
**R19 (Low)** `row_to_map` lossy on non-UTF-8 (`from_utf8_lossy`). ·
**R20 (Low)** author-supplied retry `cap-ms` unbounded. ·
**R21 (Low)** `classify` matches `Display` text — mitigated by preflight (the
string match is an optimization, not the boundary; say so in `classify` so
nobody "simplifies" the preflight away), and `invalidation_reason` is **PG17+**
so the version floor needs stating somewhere enforceable. ·
**R22 (Low)** `subject_token` collisions (`a.b` ≡ `a_b`): reject or hash-suffix
when sanitization changed the string, rather than map; do with E3. ·
**R23 (Low)** unbounded `OFFSET` in the API gateway — bounded in practice by
`statement_timeout`; keyset pagination is the end state, C5-1's stable
tiebreaker its prerequisite.

### R24–R33 — Wave-1 review pass (2026-07-19)
The §5.2 fleet's verified findings, integrated by the single integration pass;
each has a board row and a bead (wamn-2jkm.42–.51). ·
**R24 (Med)** Merge/loop flows are unresumable: the walk dispatches once per
edge (`engine.rs:438–446`; merges/cycles are stated v1 features) but
`record_node_run` writes `occurrence = 0` (`flowrunner/lib.rs:234–237`) into a
PK that collapses repeats, while `Plan::resume` (`engine.rs:529–557`) demands
one record per dispatch — a resumed merge/loop run dies with a spurious
`Mismatch`/`Overrun`. Latent (shipped fixtures acyclic); escalates to High when
a multi-visit flow needs resume. Occurrence end-to-end + visit-aware match
(wamn-2jkm.42). **Shipped `6edd545`** (with wamn-03m + wamn-cjv.10, one fix):
the engine computes `Dispatch::occurrence` from completed visits, both guests
bind it (the store builders take it as `$3` — never a literal 0 again), replay
walks the history visit-by-visit, and a legacy collapsed history now fails a
LOUD `Mismatch` instead of silently re-running recorded effects. Standing
guard: the runnerbench merge-resume phase (a delay-merge diamond parks between
merge visits; the production claim path reconstructs a partially-recorded
merge — 7 per-visit rows or the gate fails). ·
**R25 (Low)** `idempotency_key = run_id:node` is identical across visits — an
external dedupe drops a legitimate second merge arrival re-dispatched on
resume; include the occurrence (wamn-2jkm.43, with R24). **Shipped `6b525e7`**:
`run:node:occurrence` — retries keep their key, visits differ, a resumed visit
reconstructs the key it would have carried live. ·
**R26 (Low)** `resume` folds error-routed records through the Success arm
(`engine.rs:541–549`), bumping `step_seq` and `result` where live routing
touches neither (wamn-2jkm.44). **Shipped `4918df7`**: replay routes an
ERROR_PORT record through the same helper live `error_or_fail` uses —
`step_seq`/`result` untouched, occurrence still advances. ·
**R27 (High, isolation)** The slug validators permit interior `--`
(`name.rs:86–88` — test `:235` asserts `a--b` valid; registry
`validate.rs:76–82`; the DB CHECK) while `--`/`__` are the derived-name
separators: `(a, x--p, dev)` and `(a--x, p, dev)` collide on the same database
**and the same cluster-global CDC role/slot/publication** — one tenant's
reader streams the other's WAL on a shared pool. `validate_project_env` also
never slug-validates org/env. Reject `--`, validate all three components,
injectivity test (wamn-2jkm.45). ·
**R28 (Med)** The CDC credential's true blast radius is **cluster-wide WAL**
(REPLICATION has no per-database scope); the "one registration" comment
self-contradicts (`sql.rs:125–131`). Fix the claim; decide shared-pool CDC
posture (wamn-2jkm.46). ·
**R29 (Low)** `create_failover_slot_sql` guards on existence only — a
pre-existing `failover=false` slot is silently kept; apply R12's read-back
pattern (wamn-2jkm.47). ·
**R30 (Low)** The vault keeps every project's secrets as plain `String` for
the process lifetime and clones on resolve — no zeroization; log hygiene
verified clean (wamn-2jkm.48). ·
**R31 (Low)** Neither `wamn_credentials` nor `wamn_postgres` implements
`on_workload_unbind` — claims/grants survive unbind, maps grow unbounded;
must fix before wamn-bd5's per-invocation grants (wamn-2jkm.49). ·
**R32 (High)** Both flowrunner drivers advance the engine with a hardcoded
clock of 0 and turn `Step::Wait` into `Err("unexpected retry wait")`
(`lib.rs:682/731/987/1032` and `:703/:1005`): a standard http/postgres node's
`Retryable` on the production `run_next→execute_claimed` path aborts the
invocation, holds the lease to expiry, and resets `attempt` on every
reconstruction — the retry budget never advances and the error edge never
fires. The stated design (retries park via the queue layer) is unimplemented.
`Step::Wait` → park (wamn-2jkm.50). ·
**R33 (Low)** The delay node's wake is one global `state_json` key — a
two-delay flow never delays the second, and the stale wake is never cleared
(wamn-2jkm.51).

---

## 3 — Event plane (E-series)

### E13 — Egress bypass *(Critical — a claimed security property is unenforced)*
Verified in the pinned fork: `engine/mod.rs` adds `sockets::{tcp,udp,
tcp_create_socket,udp_create_socket,instance_network,network,ip_name_lookup}`
and `add_p3_to_linker` **unconditionally** — not gated by `hostInterfaces` —
and the policy closure in `engine/linked_call.rs` returns `true` for
`SocketAddrUse::TcpConnect` with no reference to `allowed_hosts`, which is
consulted only on the HTTP path. So wamn's per-flow egress allowlists
(deny-all default, `wamn:runner/egress`, the credproof gate) govern
`wasi:http/outgoing-handler` **only**: a component importing `wasi:sockets`
opens arbitrary outbound TCP, with DNS (`ip_name_lookup` is linked too).
**Fix, both layers.** *Build-time:* the builder derives grants from declared
WIT imports — add an **import denylist** rejecting `wasi:sockets*` at publish,
plus a credproof-style gate asserting refusal. *Runtime (fork commit,
adaptation class):* tighten the `TcpConnect` arm — prefer a **binary** policy
(deny unless the workload opts in via config) over allowlist matching, because
`allowed_hosts` is name-shaped while `TcpConnect` sees a post-DNS `SocketAddr`;
matching properly would also require hooking `ip_name_lookup`, and name→IP
allowlists are fragile (rebinding, shared IPs).
**Scope note:** audit the other unconditionally-linked WASI interfaces
(`wasi:filesystem` especially — volume mounts are the only bound today) against
the platform's stated sandbox claims.
**Closed 2026-07-19, both halves.** Build: `845d023` —
`wamn_host::egress_guard` (package-keyed `wasi:sockets` denylist,
`screen_component`) + the hermetic `socketguard` refusal gate, negative and
positive PASS (wamn-2jkm.8). Runtime: fork `8b76869` consumed at `627a108` —
`TcpConnect` denied unless the workload opts in (`wamn.allow-raw-sockets`
config, then `WAMN_ALLOW_RAW_SOCKETS`, else deny; unparseable denies;
warn-once per component); the fork's 4th carried commit — **at the escalation
threshold** (wamn-7j0.1). The negative runtime gate rides the wamn-2jkm.41
rebake; the deliberately-excluded UDP arms are E15/E16 (wamn-7j0.2); the
scope-note audit ran → E15–E17 below.

### E4 — `run_id` lexical vs numeric `stream_seq` *(High — before the materializer)*
D19 §5 specifies `run_id = <flow>:evt:<stream_seq>`; the queue claims on
`(available_at, run_id)` with `run_id` **text**, so `f1:evt:10` sorts before
`f1:evt:9` and per-key claim order silently interleaves — the corruption class
R6/D20 exists to prevent, arriving through a string comparison.
**Fix:** carry `stream_seq` as a `BIGINT` column ahead of `run_id` in the
ordering key (numeric semantics, indexable, no width ceiling); zero-pad to
fixed width as the belt. **Verify:** enqueue seq 8/9/10/11 on one partition key,
assert claim order is numeric — the test that fails before the fix. Free today,
a data migration later.

### E1 — Sequential publish caps capture at ~1/RTT *(High)*
`drain()` awaits each row's JetStream ack before reading the next, capping the
platform's entire capture path at one round trip per row. **Sequential-ack buys
no ordering**: NATS is ordered per connection and assigns `stream_seq` on
arrival, so pipelined publishes land in publish order regardless of ack timing
— ordering is broken by *parallel connections*, not pipelining.
**Fix:** publish without awaiting, hold the ack futures, settle them **at the
`Commit` frame** before advancing the LSN — the v3 §4 invariant is preserved
exactly, because it was always per-transaction. Bound the in-flight set (e.g.
256) and drain mid-transaction when hit (safe; the LSN still holds). On failure,
retry the transaction's publishes from the first unacked row; the dedupe window
absorbs the landed prefix and `ON CONFLICT` absorbs what it misses.
**If insufficient** (C-CDC decides): shard across M connections **hashed on
partition key** — per-key order preserved, cross-key parallel. **Never**
unordered publish with consumer-side reordering: it needs a watermark/gap
protocol and destroys the monotonic-`stream_seq`-vs-WAL property the queue
depends on.
**Dependency: R12 first.** Pipelining raises in-flight unacked from 1 to ~256,
so a crash republishes a longer prefix more often — the recovery story is the
dedupe window plus the materializer's `ON CONFLICT`, and R12 is what makes the
window's configuration *asserted* rather than hoped. Do not land this ahead of
R12's stream-config assertion.

### E2 — Reader stall is silent *(High — safety interlock, not metrics)*
`publish_acked` retries forever with 10 s cap, emitting identical warns —
nothing distinguishes two retries from six hours. Meanwhile the LSN is held
**by design**, so an unreachable JetStream silently freezes WAL retention on the
customer's database until `max_slot_wal_keep_size` invalidates the slot, which
is a **capture gap** — the worst incident in this architecture. "Delayed never
lost" is correct and only *safe* if someone is told early.
**Fix:** (1) metrics per project-env — `publish_retries`,
`publish_stall_seconds` (age of oldest unacked), `confirmed_lsn_age_seconds`,
`events_published`, `reopens`; (2) escalating levels + a distinct
`CDC_PUBLISH_STALLED` event past a threshold (default 30 s) for alerts to bind
to; (3) **the real backstop** — poll `pg_replication_slots.safe_wal_size` and
publish `slot_safe_wal_bytes`, alerting *before* `wal_status` leaves
`reserved`. Runbook line: on sustained stall, fix JetStream — **do not drop the
slot** (that "fixes" the disk by creating the gap).

### E10 — `wasmcloud:messaging@0.2.0` cannot carry the materializer *(High, verified)*
The only messaging interface in the pinned fork is
`wasmcloud-messaging-0.2.0` (`wasi:messaging` is **not** implemented — it
appears once, as a test string in `wit.rs`). Its whole surface:
`broker-message { subject, body, reply-to }`, `handler.handle-message`,
`consumer.{request, publish}`. Absent: ack/nack, ack floor, durable-consumer
config, pull consumers, redelivery count, `stream_seq` — **and headers**, so a
component cannot set `Nats-Msg-Id` and therefore cannot participate in
JetStream dedupe *on either side*. The implementation matches
(`plugin/wasmcloud_messaging/nats.rs` is core NATS; zero JetStream). There is a
backend-extension seam, but **the WIT is the binding constraint**.
**Fix:** define **`wamn:jetstream@0.1.0`** — a *new* package (never a forked
`wasmcloud:messaging@0.2.0`; namespace collisions are worse than new
namespaces), host plugin over the async-nats JetStream client in the
`wamn:postgres` shape, carrying durable-consumer binding, pull/fetch with
bounded batch, ack/nack/term, redelivery count, `stream_seq`, and headers both
directions. This is a genuine D17 bar-clearing case with a verified citation.
**Ledger exit condition:** upstream `wasmcloud:nats` (#5065) landing with
durable-consumer + header semantics.

### E11 — Native-service drift; adopt the default rule *(High, posture)*
The event pipeline runs entirely outside the model sold to tenants: dispatcher
(justified — multi-org credentials), CDC reader (justified — see E12),
run-worker (*was* "pending #5336"; **corrected by E12** — fixable now),
materializer (**no constraint at all**). Tenant-facing pieces are properly
components. The line — *components for tenant execution, native for platform
machinery* — is defensible and maps onto wasmCloud's own control/data-plane
doctrine, but nobody decided it; it accreted.
**Not drift, for the record:** NATS/JetStream are wasmCloud's own substrate
(the Helm chart supports separate control/data URLs; 2.0 removed JetStream from
*scheduling state*, never messaging), per-org streams are the Posture-C
on-ramp, and CDC is orthogonal. Infrastructure is aligned; *service topology*
drifted.
**Rule (D-row):** *New platform services are components — as `Service`
workloads (E12) — unless a recorded exception names the constraint. Two classes
qualify: no wasm32 client library exists for a required protocol (the reader),
and multi-org credential scope (the dispatcher). Interface absence is not a
constraint — it is an argument for a `wamn:*` WIT (E10). Deployment-shape doubt
is not a constraint — `Service` workloads express long-lived loops today.*

### E12 — `Service` workloads already exist in 2.5.2 *(High — corrects E11)*
`wash-runtime/src/types.rs`: `Workload { service: Option<Service>, components:
Vec<Component>, host_interfaces, … }`, `Service { bytes, digest,
local_resources, max_restarts }`, exposed in the `Workload` CRD
(`spec.service`). The CRD's own words: *"A Service differs from a Component in
that it is long-running… Services export a single WIT interface, shaped as
`wasi:cli/run`. Services can import interfaces from any Component within the
same workload, or from the Host."* Port policy (`linked_call.rs`):
`TcpBind if is_service => is_loopback`, `TcpBind => false`, `TcpConnect =>
true`. *(Two `service` fields exist in the CRD — one references a K8s Service
for EndpointSlice/DNS; don't conflate.)*
**Shapes available today, no upstream dependency:** **run-worker → `Service`**
in a Workload whose `components` include the **flowrunner** — the CRD's
documented pattern — which also frees the flowrunner from the host image and
restores independent rollout; **materializer → `Service`** importing
`wamn:jetstream`; **dispatcher →** re-examine (a `Service` per org is now
expressible, and per-org credentials is the R8b direction anyway).
**Reader — why it stays native, stated correctly after two wrong attempts.**
*Retracted:* ~~"no `wasi:sockets` plugin"~~ (sockets are not a plugin;
`src/sockets/` implements p2 **and** p3 and is linked unconditionally) and
~~"just a transport rewrite"~~. The `--no-default-features` build of
`pg-walstream` exports `protocol::{LogicalReplicationParser, …}` plus encoder
and LSN types — a **byte decoder**; everything the reader calls
(`EventStream`, `LogicalReplicationStream`, `ReplicationStreamConfig`,
`StreamingMode`, `WalRouter`, `RawXLogData`) is behind
`#[cfg(any(feature = "libpq", feature = "rustls-tls"))]`, and all of
`src/connection/` is feature-gated. A component reader means writing the
Postgres startup packet, **SCRAM-SHA-256**, **TLS** (rustls+aws-lc-rs needs
cmake/gcc and won't cross-compile; it would need `wasi:tls`, a wash-runtime
feature this fork does not enable), `IDENTIFY_SYSTEM`/`START_REPLICATION`,
CopyBoth, and the **standby-status feedback loop** — i.e. owning a hand-rolled
protocol client on the most security-critical path, for nothing gained.
**Exception wording:** *native because no wasm32 Postgres replication client
exists.* **Exit:** such a client, or `pg-walstream` gaining a `wasi:sockets`
transport.
**Implementation:** build each as `wasi:cli/run`; declare host imports in
`host_interfaces`; set `max_restarts`, limits, `allowedHosts` in the CRD;
delete the hand-rolled Deployment YAML. **Materializer first** (greenfield —
costs nothing to start correct, validates the shape before the run-worker
migration touches working code). `failoverbench` must then re-run under
**operator**-initiated restarts, not just pod kills.

### E14 — Q1 resolved: `ev.lsn` is per-message *(closed)*
If `pg-walstream` returned the *transaction* LSN, every row after the first in
a multi-row txn would share a `msg_id` and JetStream would dedupe them away —
silent loss, invisible in every metric. **It does not:** `stream.rs:1093` calls
`convert_to_change_event(msg, raw.wal_start.value())` and `wal_start` is parsed
from the **XLogData frame header** (`:1066`), so per-event LSNs are distinct by
construction; `StreamingMode::Off` rules out the v2+ streaming edge.
**Standing guard:** a `streambench` assertion that published-event count ==
distinct `Nats-Msg-Id` count over a run containing a large multi-row
transaction. `poc/cdc1` establishes *monotonicity*; this establishes
*distinctness*, which is what `msg_id` depends on. **Shipped** `7440d33`
(wamn-l5i9.55): txn-shaped 200-row batch (dense per-event LSNs, one XID),
client-side pairwise-distinct msg-ids + server-side stream-delta == N with
JetStream dedupe as the silent-drop detector; runs under `--mode publish`/`all`.

### E3, E5–E9 — remainder
**E3** `entity` is an unqualified table name (publication is single-schema
today, so names are unique — but carry `schema` in the envelope now, before
`l5i9.11` makes catalog-entity id the subject token and registration key) ·
**E5** TRUNCATE — **deferred**, see §5.3 · **E6** `ceilings.md` banner (**closed** `9ea8da0`, wamn-2jkm.3)
(`fsync=off` figures are shape-only; C7 measured the run queue, which survives
the CDC pivot, so the number stays live — C2 is the one whose subject
disappears) · **E7** the reader is a long-lived service living as a CLI
subcommand → extract with **E8** reader placement/ownership (slot exclusivity
means one session per project-env; today one hand-launched process each;
recommend a multi-tenant reader sharded by a system-DB lease — the dispatcher's
proven model — with a per-org isolated reader as the escape hatch, and a
**"registered but not running" alarm**, because that state is invisible in
every other metric) — **E7 closed** `f044b5f` (wamn-l5i9.48; extraction shipped
with SR9, remainder = `event-reader-rbac.yaml` zero-grant identity + credential
scope on the example) · **E8 closed as D22** `055dfe6` (wamn-l5i9.46 ratified
2026-07-19; the alarm stays open as E8b/wamn-l5i9.47) · **E9** — **canonical home is §1.3** (archive moves; **closed** `db4d891`, wamn-2jkm.2).

### E15–E17 — Wave-1 audit additions (2026-07-19)
**E15 (High, wamn-7j0.2)** Raw **UDP** egress is allow-all:
`UdpConnect`/`UdpOutgoingDatagram` return `true` unconditionally
(`linked_call.rs`; enforced call sites `host_udp.rs:136,533`) — the E13 bypass
on UDP, deliberately left by the E13 fork commit (TcpConnect only). Deny by
default behind the same opt-in. ·
**E16 (Med, wamn-7j0.2)** `UdpBind` allows loopback **or unspecified** for
every component — an all-interfaces inbound UDP listener its TCP sibling
forbids (`TcpBind => false` for non-services). Align the arms. ·
**E17 (Med, latent — wamn-2jkm.52, hard precondition of wamn-bd5)**
`egressbench`'s classifier allowlists `wamn:postgres` and asserts custom-node
shapes with `require_postgres=false` (`egressbench.rs:53,158–161`) — a tenant
node importing `wamn:postgres/client` would PASS the named publish-gate
backstop and receive the raw DB surface (and the DO/EXECUTE claim-mutation
bypass). Enforce the positive allowlist; wire `screen_component` (E13a) into
the same chokepoint. ·
**E13 precision (from the audit):** `ip_name_lookup` is linked but
**default-denied** (`allowed_network_uses.ip_name_lookup = false`, enforced at
`host_ip_name_lookup.rs:38`), so E13's "with DNS" overstated — the raw-IP
connect path needs no DNS. Verified consistent elsewhere: filesystem preopens
= validated volume mounts only; env from `local_resources.environment` only;
stdin empty; `proc_exit` traps the component, not the host.

---

## 4 — Structure & quality (SR-series)

### SR11 — Positional SQL parameters compose across crates with no type *(High)*
`run-queue/src/sql.rs:214` hardcodes `$7`/`$8` on the assumption that
`wamn_run_store::sql::insert_node_run_success_sql()` — **a different crate** —
uses exactly `$1..$6`. Same shape at `record_error_and_renew_sql` and
`complete_dequeue_sql`. Add one parameter upstream and this **misbinds
silently**: lease TTL and owner guard shift by one, on the per-node checkpoint
path every run executes. The type system cannot see it; no test in
`wamn-run-queue` can, because the coupling is to a string produced elsewhere.
The only guard is a comment. This is the bill for "pure crates emit SQL
strings": the strings compose, their **contracts** do not.
**Fix:** `struct Sql { text: String, arity: u16 }` with `append(tail,
tail_arity)` renumbering the tail against the head's arity (safer), or exposing
`base.arity` so callers write `${base.arity + 1}` (one afternoon). Put it in
`wamn-ddl` or a new `wamn-sql` leaf; convert the three composing call sites
first; leaves may keep returning `String`. **Verify:** assert `arity` per
composed statement against a pinned constant.

### SR12 — The pure/effect split cannot test the bug class that bites *(High)*
Pure crates emit SQL as `String`, which makes the **decision** testable and
leaves the **statement** untested — the model has no planner, isolation level,
lock manager, or RLS. **Proof from this tree:** `plan_claim` modelled the batch
claim correctly and passed while the real `claim_batch_sql` over-claimed on a
`LIMIT 1` (plan-dependent `SKIP LOCKED` re-scan); the fix (`AS MATERIALIZED`)
is a property of the emitted SQL no pure test can observe, and it surfaced only
through the plugin's cached prepared-statement path. Same blind spot covers
isolation assumptions (the dispatcher's in-transaction registry re-read is
reasoned about in a comment, not tested), lock ordering, RLS, `ON CONFLICT`
races, index selection.
**Not an argument against the split** — it is what makes `wamn-runner`,
`wamn-catalog`, `wamn-ddl` tractable. It is an argument for: **(1)** qualify
the crate headers — *"decisions are unit-testable; statements are not"*
(cheap, do now); **(2)** extend the existing `WAMN_*_PG_URL`-gated live tests
from DDL apply to the **claim/queue SQL**, where plan-sensitivity actually
bites — *not* a second harness, `wamn-gates` already covers this ground and the
gap is the trigger; **(3)** annotate every composed or plan-sensitive statement
with what the pure test does not cover, the way `claim_batch_sql` now does —
make that comment the convention.

### SR13 — Two sources of truth for schema *(Med)*
Tenant tables compile from the catalog via `wamn-ddl`; platform tables are
**hand-written SQL** in `deploy/` (~1,425 lines across seven files). Pure crates
emit SQL naming those tables and nothing checks agreement — the symptom is
already visible in `PartitionPolicy::as_sql`'s comment that its literals are
"drift-guarded against the `deploy/sql/run-queue.sql` CHECK", a bespoke manual
guard for one enum patching a hole that exists for every column.
**Fix — the repo already applies the right pattern elsewhere:**
`catalog-model.schema.json` and `flow-schema.schema.json` are *generated* from
Rust with a drift test. Either compile `deploy/*.sql` through `wamn-ddl`
(stronger, larger decision) or generate them from Rust and check the artifact
in with a drift test (less invasive, buys column-existence for free).

### SR9 — `wamn-host` is three programs in one crate *(Med)*
See §1.7b. Split by **deployment artifact**: `wamn-host` (runtime: engine,
host, plugins — lib + thin bin), `wamn-ctl` (the ten one-shot verbs, subcommand
surface unchanged so Job manifests are a `command:` swap), and **per-service
binaries** — `wamn-dispatcher`, `wamn-run-worker`, `wamn-cdc-reader` — each with
its own image target, Deployment, RBAC, and credential scope. That last part is
exactly what E7/E8 need for the reader, so do it as one change; and note E12
changes the *destination* for run-worker (a `Service` workload) and materializer
(likewise), so only the dispatcher and reader remain native binaries.
Image targets follow SR1's pattern (one Dockerfile, `--target` per artifact);
the washlet image must stop carrying provisioning and replication-credential
code (`strings` spot-check, per SR1's precedent).

**Closed 2026-07-19** (wamn-2jkm.22, solo main-loop): `d4fe3aa` identifiers →
`wamn-registry::identifiers` (the R16b owner relocated, still singular, so the
dispatcher links no runtime) · `7262679` `wamn-ctl` (nine verbs — the bead's
"ten" counted the private `env_policies` helper; `--help` byte-identical to the
pre-split baseline modulo binary name) · `157b61b` the three service crates
(`wamn-dispatcher` wasmtime-free via a local thin `connect_nats`/`init_crypto`
copy; `wamn-run-worker` embeds the runtime via the lib; `wamn-cdc-reader`
standalone, `event_reader_live` moved) · `685a7fc` six Dockerfile targets +
manifest swaps. Strings spot-check held on the SHIPPED release binaries: the
washlet carries zero provisioning/replication-credential markers. Gates: 89
workspace test binaries green, all six image targets built locally; the
in-cluster rollout of the six images rides `wamn-2jkm.41` (rider 11). E7/E8
were NOT taken (not owner-named): E7 `wamn-l5i9.48` is now unblocked on this
layout; the E8 placement decision is `wamn-l5i9.46`.

### SR1–SR8, SR10 — status
**SR1** gates split *(closed)* · **SR2** flowrunner re-implements run-state SQL
that `wamn-run-store` owns — single pure SQL source, guest-compilable; target
≤ ~400 lines of dispatch glue *(open, before F3/F4)* · **SR3** repo tiering
*(closed — and it **held**, which is evidence SR6 works)* · **SR4**
`wamn_postgres.rs` module split — **1,510 → 1,788 lines (+18%) since filing**,
evidence that a filed-but-unscheduled structural finding is not a brake; do it
with R2/R16, which touch the same file *(open)* · **SR5** `CronError(String)`
→ structured variants *(**closed** — `ca59e0e`, wamn-qfr.4; board corrected
2026-07-19: the ledger merge failed to carry structure-review's Done forward —
the inverse of the R8c error, caught by the R10 closure audit)* · **SR6** conventions written down
*(closed)* · **SR7** WIT vendoring consolidation *(open, opportunistic — the
coherence test means no correctness exposure)* · **SR8** `deploy/` tiering
*(open — §1.6 **is** SR8; this line is a pointer only)* · **SR10** `wamn-gates` `{bench,proof,fixture}/`
submodules; audit helper duplication against `wamn-gate-harness` (541 lines
serving 29 modules suggests the duplication SR1 removed is re-accumulating)
*(open, next bench)*.

---

## 5 — Deferred, declined, and open questions

**5.1 Open questions — all three RESOLVED 2026-07-19** (question-class
closure on cited evidence; full citation sets in the wamn-2jkm.15/.16 and
wamn-l5i9.56 close records): ·
*Q1 — is `wamn-node-guest --features caps` tenant-reachable?* **No.** Two
independent verified gates: the `hostInterfaces` CRD allowlist is
operator/control-plane authored and no tenant deployment path exists (the 5.5
builder and custom nodes are unshipped P2), and the 5.6 design excludes
`wamn:postgres` from the custom-node import allowlist outright
(`wamn-node-design-notes.md:64–77`; `f1-workloads.yaml:5–8`;
`node-library.md:86–93` — the raw-SQL facade hard-returns false until
`wamn-1nd`). The RLS boundary is therefore **not** presently bypassable
in-band; `wamn-1nd` stays future conditioning, priority unchanged. The review
also minted **E17**: the shipped `egressbench` classifier would PASS a tenant
node importing `wamn:postgres` (`egressbench.rs:53,158–161`) — the mechanical
allowlist this verdict delegates to is not yet enforced (wamn-2jkm.52). ·
*Q2 — does anything set `REPLICA IDENTITY`?* **No — confirmed exhaustively**
(provisioning, DDL compiler, reader: the string appears only in bench/test/doc
comments), so the de-facto contract is **DEFAULT over the surrogate `id uuid`
PK** (`emit.rs:149`): full `new` on insert/update with unchanged-TOAST columns
*absent* (distinct from a real null); **no `old` on update** (the PK never
changes); key-only `old` (`id` alone — not even `tenant_id`) on delete.
Verified live (`event_reader_live.rs:619–645`; the `poc/cdc1` TOAST test).
Sufficient for the `ON CONFLICT` enqueue (stream_seq-keyed) and new-value
conditions; **insufficient** for old-value/changed-to conditions and delete
payloads — exactly what `l5i9.31` fills. `ALTER … REPLICA IDENTITY FULL` is
**non-retroactive**, so the pre-FULL window is a permanent old-image gap that
grows with capture time: `l5i9.31` must apply at/before registration **before**
the materializer exposes old-value conditions, and old-absent must read as
"cannot evaluate", never "condition false" (design paragraph appended to
wamn-l5i9.17). ·
*Q3 — is `wamn_dispatch` `NOBYPASSRLS` non-owner as deployed?* **The role does
not exist and never did** — the dispatcher connects as `wamn_app`, which IS
`NOBYPASSRLS`, non-super, and a non-owner under `FORCE ROW LEVEL SECURITY`,
verified statically (`postgres-init.sql:13`; `wamn-provision/sql.rs:30–42`;
`run-queue.sql:78–83`) **and live** (pg_roles/pg_class on the fixture). R8b
splits: **R8b-a** (a dedicated narrowly-granted dispatcher role — `wamn_app`
holds DML on tenant data tables and can `SET app.tenant` freely, so a leaked
credential reads any tenant) stays open = **wamn-286**; **R8b-b** (explicit
tenant predicate on the **four** RLS-only statement families —
`outbox_poll_sql`, `parked_due_sql`, `active_flows_sql`, `global_claim_cte` —
not just the two R8b named) = **wamn-2jkm.53**, close-as-defence-in-depth.

**5.2 Never-reviewed surface — reviewed 2026-07-19 (Wave 1,
wamn-2jkm.11–.14).** All four surfaces got a read-only correctness pass:
**`Plan::resume`** — which is the **flow runner's** branch-aware resume
(`wamn-runner/src/engine.rs:522–560`), *not* the migration planner's as this
list previously said — yielded R24–R26; **`wamn-provision` + `wamn-registry`**
yielded R27–R29 (the SQL-injection surface verified well-defended:
`quote_ident`/`quote_literal` drift-tested byte-for-byte, `$n` params
everywhere, NAMEDATALEN bounds tested); **`wamn_credentials.rs`** yielded
R30–R31 (grant-check fail-closed ordering, TOCTOU, tenant scoping, and log
hygiene verified sound); **`components/flowrunner`** yielded R32–R33 (the
causation declare path, `will_error_route` mirroring, checkpoint ordering, and
SQL column-order parsing verified sound). Coverage-rate observation
from the two independent 2026-07-18 passes: `event_reader.rs` yielded 11
findings with exactly **1 overlap** between passes, which estimates the unfound
surface as comparable to the found. That observation is also the strongest
standing argument for the declined item below.

**5.2b Declined by owner:** CI and LICENSE. Recorded, not argued: the
evidence-based re-open case is that R11 (missing backoff), R13 (a `clamp` panic
reachable from a flag), and SR4's 18% growth are each things
`clippy -D warnings` + one idle-path test + a line-count threshold would have
caught unattended, and that the repo's quality argument rests on a gate suite
nothing triggers (see SR10: `wamn-gates` is 27.8% of all Rust, a hand-run
harness outside `cargo test` that no automation invokes). Related nit if it is ever revisited: two `#[allow(...)]` sites
should be `#[expect(...)]` per M-LINT-OVERRIDE-EXPECT, and `POOL_SLOTS = 512` ×
`max_memory_size = 256 MiB` reserves ~144 GiB of virtual address space — fine
on 64-bit Linux, fails under `RLIMIT_AS` or `vm.overcommit_memory=2`, worth a
line next to the kind/helm instructions.

**5.3 Deferred by owner — TRUNCATE — updated 2026-07-19: the stated blocker
is resolved.** The prior note deferred on *"is `TRUNCATE` permitted on tenant
tables at all?"* — **the code already answers no**: the app-role grant is
`SELECT, INSERT, UPDATE, DELETE` only (`wamn-ddl/src/emit.rs:187`; TRUNCATE was
never granted, confirmed independently by `walbench.rs:825`'s own comment), the
CDC role is `SELECT`-only (`wamn-provision/src/sql.rs:207`), and the migration
engine's destructive path is `DROP` + rename-aside, not TRUNCATE. So the
sentinel-vs-exclusion conditional collapses: **exclude-at-capture is correct**
(`WITH (publish = 'insert, update, delete')` + `ALTER PUBLICATION` migration),
and the residual producers are **platform machinery and operators** — real
today: `walbench.rs:829` TRUNCATEs published tables as admin, so a bench run
against a CDC-enabled env produces the divergence now, and v3 §1 scopes psql
in. That argues for exclusion **plus an operator alert** (the reader's
`Truncate` arm becomes a counted incident meaning "publication drifted or an
operator truncated a captured table"), not a consumer-visible sentinel.
**Status: still deferred by owner decision — but it is now a one-line sign-off
on a worked answer, not an open design question.**

---

## 6 — Sequencing

> **⚠ Parallel-editing warning — read before spawning worktrees or agents.**
> **This file is the contention hotspot.** Parallel work streams MUST NOT edit
> `findings.md` (or `docs/` generally, outside their assigned scope): agents
> close findings in **commit messages carrying the finding ID**
> (`fix(R13): …`), and a single integration pass sweeps the status board
> afterward — which is also what the closure rule requires (evidence first,
> board second). Two further shared resources serialize: the **in-cluster gate
> suite** (one cluster ⇒ one runner at a time; either a kind cluster per
> worktree or gates only at merge) and the **wasmCloud fork** (its own repo —
> conflict-free with wamn work, but rebuilding the host image invalidates
> everyone's running cluster). And the standing caution: parallel agents
> *amplify* the failure mode this review cycle kept catching — closing against
> intentions rather than commits. The closure rule is what makes parallelism
> safe; it is not optional under parallelism.

### Wave structure (worktree/agent assignment)

**Wave 0 — solo, first (merge-conflict magnet).** All of §1 (docs
reorganization, index, archive moves, D4 line) + R10's rule adoption + board
corrections. Everything downstream cites IDs and paths this wave creates.
~Half a day.

**Wave 1 — parallel, one worktree each, no file overlap:**
- **Reader cluster** (one agent, serial within): R11 backoff ladder + E2
  stall/slot metrics + R12 stream-config assertion — all `event_reader.rs`;
  they are one review.
- **R13**: `Cadence::new` validation — `wamn-run-queue/dispatch.rs` +
  arg-parsing, isolated.
- **E13 build-time**: builder import denylist + refusal gate — builder crates
  only.
- **E13 runtime**: `TcpConnect` policy commit — **the fork repo**, zero wamn
  contention.
- **E10/E11 sitting**: `wamn:jetstream@0.1.0` WIT draft + D-row + posture
  rows — new files and docs, no code conflict.
- **Review agents (read-only, embarrassingly parallel):** the §5.2
  never-reviewed surface — `Plan::resume`, `wamn-provision`+`wamn-registry`,
  `wamn_credentials.rs`, `components/flowrunner`. Findings come back as
  candidate ledger sections; zero merge risk.

**Wave 2 — after Wave 1 merges** *(executed 2026-07-19: `35a8bff` E1 · `709d2cf` E4 · `cebd722` R14 · `7b4671f` SR11 · `79e414b` R8b-b · `c705c9e` SR12b · `f7652c6` R2/R16 · `e235abb` R16b · `d770302` R18 · `7f91e3a` SR4; riders per §6 "with the next queue work" included)*:
- **Queue cluster** (one agent): E4 `stream_seq` ordering + R14 held-row
  exclusion + SR11 `Sql` arity type — all `wamn-run-queue` and neighbors.
- **E1** publish pipelining — requires the reader cluster merged (R12 is its
  stated prerequisite) and the C-CDC bench slot on the shared cluster.
- **R16/R2 + R18 + SR4** — one sitting, all `wamn_postgres.rs`.

**Wave 2.5 — open-Highs sitting (owner-inserted 2026-07-19, before the sync
point; near-disjoint subsystems) — EXECUTED 2026-07-19:** R32 retry-abort
(`wamn-2jkm.50`, `a59619d`) ∥ E15/E16 fork UDP arms (`wamn-7j0.2`, fork
`eef76cd8` + pin `4e82c8f` — **5th carried commit, escalation threshold
reached: engage upstream**) ∥ R27 slug injectivity (`wamn-2jkm.45`,
`0d560b6`) ∥ E17 egressbench positive allowlist (`wamn-2jkm.52`, `91659ff`
— unblocked wamn-bd5) · SR12a header qualification (`wamn-2jkm.17`,
`0d7231f`, main-loop solo after the merges). Four parallel worktree agents,
serial main-loop integration, zero cherry-pick conflicts.

**Sync point — anti-parallel by nature:** SR9/E7/E8 (the `wamn-host` crate
split + reader extraction). It touches every deployment artifact and wants a
quiet tree; schedule it alone between waves, then the materializer build
(as a `Service`, E12) proceeds on the new layout. — **SR9 EXECUTED 2026-07-19**
(`d4fe3aa`…`685a7fc`, solo main-loop per the owner's "take sr9 on resume");
E7 (`wamn-l5i9.48`, unblocked) and the E8 decision (`wamn-l5i9.46`) remain,
owner's pick. — **MATERIALIZER-GATES WAVE EXECUTED 2026-07-19** (owner
authorized "fqg.20 ∥ l5i9.16 … ratify E8 … fold E7's remainder in"): E8
ratified as **D22** (`055dfe6`) · E7 remainder closed (`f044b5f`) ·
`wamn-fqg.20` flow-level ordering + dispatcher key stamping (`c32ffaf`) ∥
`wamn-l5i9.16` EVT-REG registration catalog + minimal API (`b456409`) — two
parallel worktree agents, serial main-loop integration, zero conflicts. The
materializer (`wamn-l5i9.17`, Service-first) is now fully unblocked; the
single six-image rebake sweep is `wamn-2jkm.41`.

**Day one (~half a day, documentation only, no code risk).** §1.1
`docs/README.md` · §1.2 the D4 supersession line · §1.3 archive moves · R10's
closure rule into this doc's header and `AGENTS.md`, and reopen R8c · §1.6
`deploy/` `git mv` batches.

**This week (cheap code, real failure modes behind them).** R13 (ten lines,
removes an idle-path panic) · E13 build-time half (one import-denylist rule) ·
E4 (`stream_seq` numeric ordering — free now, a migration later) · E2 metrics +
slot-headroom gauge, and R11's backoff ladder — **before the Phase-1 staging
soak runs unattended**, since both are "the reader fails quietly for a long
time."

**Before the materializer is written (one sitting).** E10's `wamn:jetstream`
WIT draft · E11's default-rule D-row · E12's `Service` shape — these together
decide whether the next service is the platform's fifth native process or the
component that proves the model. Then **build the materializer as a `Service`
first**, and migrate the run-worker after.

**Before the Phase-2 cutover — in this order.** **R12 first** (stream config
assertion; E1 depends on it — see both bodies) · then E1 (publish pipelining +
the C-CDC measurement) · E7/E8 with SR9's first slice (reader as its own binary
and its placement/ownership model) · E13's runtime half.

**Next tranche (structural, decide first).** SR11 (`Sql` arity type — before the
next composed statement) · R16/R2 (`set_config` binds + validator
consolidation) **+ R18** (`standard_conforming_strings` assert) with SR4 (all
the same file — one sitting) · SR12's header qualification now and its
live-test extension with the next queue work · R14 while the outbox ships.

**Opportunistic.** SR13 with the next platform-schema change · SR10 at the next
bench · SR2 before F3/F4 · SR7 · R5/R7/R8a/R8b/R8d/R9a–c, R15, E3, E6
as their subsystems are next touched (E9 lives in day-one §1.3) · E14's distinctness assertion when
`streambench` is next opened · §1.5 `build-and-test.md` re-keying.

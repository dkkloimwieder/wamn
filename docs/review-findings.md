# Review Findings — Issues R1–R5

Source: external code review of the wamn repo (2026-07-11, tip `155ac4b`), cross-checked against the P0 results and the planning corpus; amended same day after review of the 5.14 follow-up commits (`75d8277` partition ownership, `a7d2ad2` failover — tip `5107268`); amended 2026-07-12 after review of the dispatcher tranche (`a3fb0b3` shared trigger dispatcher, `98ba290` flow-id slugs, `9123e13` production deploy, `b687d45` outbox trigger producers); amended again after review of the migration-ordering commit (`873c3d8` name-freeing preamble — tip `873c3d8`). CI/LICENSE finding excluded by decision. Each issue: context → evidence → design → implementation → verification → doc closure. Repo `docs/` are canonical; all doc-closure edits land there.

Status of R4's prerequisite: the fork exists and the tree is on **2.5.2 + the epoch commit**; R4 below is the remaining Cargo.toml switch and the standing sync runbook.

| # | Title | Severity | Gates |
|---|---|---|---|
| R1 | Park/wake consumes the redelivery budget | **High — correctness** | P1 (blocks any delay/parked flow in production) |
| R2 | Claim injection: replace validated interpolation with `set_config` binds | Medium — hardening | Before the `format!` pattern propagates; with 2.2 production work |
| R3 | Per-component memory limits (`ResourceLimiter`) | Medium-high — isolation promise | Before P1 multi-tenant density work |
| R4 | Fork-based upstream management (Cargo.toml + sync runbook) | Process | Immediate (in progress) |
| R5 | RLS claim-shape hardening + scope honesty | Low — defense-in-depth | With 3.5/4.2 work |
| R6 | `partitioned(key)` ordering under retry/park — decide, don't inherit | **High — semantics** | Before the first partitioned flow ships (5.11) |
| R7 | Failover status-flip alerting + two-lease failover latency | Low — operational notes | With 9.10 alerting / run-queue doc touch |
| R8a | Cron anchor vs run-history retention — latent duplicate fire | **Medium — latent correctness** | Must be decided before 9.6 retention ships |
| R8b | Dispatcher DB role scoping (`wamn_dispatch`) | Medium — hardening | With the next dispatcher/deploy touch |
| R8c | Outbox write-amplification + GC verification | Medium — scale | Before bulk-import tooling (3.6); GC check now |
| R8d | Cron misfire collapse — document the contract, policy knob later | Low — semantics | Doc now; knob rides 5.11-adjacent backlog |
| R9a | Reserve the `wamn_` identifier prefix at catalog validation | Low — hardening/UX | With the next 3.1 catalog-validation touch |
| R9b | Table rename × row-event registration — silent trigger loss | **Medium — silent breakage** | Named case for 11.8 impact analysis; interim doc warning now |
| R9c | One-transaction apply assumption — record the expiry (CONCURRENTLY) | Low — standing constraint | Doc now; residue janitor when 3.2 grows non-txn steps |

---

## R1 — Park/wake consumes the redelivery budget (`wamn-run-queue`)

### Problem
`attempts` is meant to bound *crash redeliveries*; it currently counts *every claim*. `claim_batch_sql` does `attempts = q.attempts + 1` unconditionally on claim (`sql.rs:57`), and `park_sql` (`sql.rs:105`) releases the lease (`lease_owner = NULL, lease_expires_at = NULL`) without touching `attempts`. Every park→wake cycle therefore burns one unit of redelivery budget. Once `attempts >= max_attempts`, `claim_state` classifies the row `Exhausted`, the claim path skips it, and the janitor (`sql.rs:122`) retires the run to `infrastructure-failure`.

**Failure narrative:** a flow with six `delay` segments and `max_attempts = 5` can *never complete* — it is retired as an infrastructure failure after its fifth wake, having failed zero times. Any long-lived parked pattern (S6's 24h delay, escalation timers, F3-style scheduled waits) hits this. This is a correctness bug in the exact mechanism S6 validated, masked in P0 because the bench parked once.

### Design decision
`attempts` counts **crash evidence only**: increment on claiming a row whose previous lease *expired* (a reclaim — the prior owner died without completing, parking, or dequeuing). Fresh claims — `lease_expires_at IS NULL`, i.e. first dispatch or a post-park wake — do not consume budget. The discriminator is already in the claim predicate (`lease_expires_at IS NULL OR lease_expires_at <= now`), so the fix is a `CASE` on which arm matched:

```sql
attempts = q.attempts + CASE WHEN q.lease_expires_at IS NOT NULL THEN 1 ELSE 0 END
```

(inside the claimed-row UPDATE, where the predicate has already established `lease_expires_at IS NULL OR <= now`; `IS NOT NULL` therefore means "expired lease" = crash evidence).

**Semantics after the fix:** `max_attempts` = "how many times may a runner die holding this run before we stop retrying" — crash-loops still retire (each reclaim increments; budget spent → claim path abstains → lease ages out → janitor sweeps, the existing race-resolution logic unchanged), while a run may park unbounded times for free.

**Alternatives rejected:**
- *`park_sql` resets `attempts` to 0* — loses crash history across park boundaries: a run that crashes, is reclaimed, parks, wakes, crashes, … never exhausts. The budget must survive parks.
- *Separate `wake_count` column* — more state for no decision the platform needs to make; wakes are free by design, not merely differently budgeted.

### Implementation
1. `sql.rs::claim_batch_sql` — the `CASE` increment above.
2. `sql.rs::claim_partition_head_sql` — **the same `CASE`** (commit `75d8277` added this second claim path with the same unconditional `attempts = q.attempts + 1`; both paths must carry crash-evidence semantics or partitioned flows keep the bug). The partition path sharpens the urgency: head-first + one-in-flight means a parked partitioned run is re-claimed on *every* wake, so delay-heavy partitioned flows burn budget fastest of all.
3. `claim.rs` pure layer — `Claimed`'s post-increment `attempts` computation gains the same conditional; `claim_state`/`is_claimable` are unchanged (`Exhausted` classification still `attempts >= max_attempts`); `plan_claim` **and `partition.rs::plan_partition_claim`** mirror the new increment so the pure models keep matching the SQL.
4. **Gate expectations:** `failoverbench` asserts `attempts == 2` after the kill→reclaim cycle; under crash-evidence semantics that becomes `1` (first claim free, expired-lease reclaim counts). Update the assertion *with* the semantics change and state why in the gate's comment — the new value is the point, not a regression.
5. Doc comments in both files + `docs/run-queue.md` lifecycle step 3: state the crash-evidence semantics explicitly, including the first-dispatch case (first claim is not a redelivery; a first-dispatch crash costs its unit on the *reclaim*).

### Verification
- Unit (`tests/queue.rs`, pure layer): (a) N-park flow with `max_attempts = 1` remains claimable at every wake; (b) crash-loop — repeated expired-lease reclaims — exhausts at exactly `max_attempts` and lands `Exhausted`; (c) interleaving: claim → park → wake-claim → crash → reclaim yields `attempts = 1`, not 3.
- Integration (`queuebench` new phase): a flow parking 10× with `max_attempts = 3` completes; janitor sweep after the run confirms no `infrastructure-failure` rows; the existing crash-loop retirement phase still passes unchanged. Repeat the parking case through the **partition path** (a partitioned run parking 10×) — the head-first re-claim per wake is where the budget burns fastest.
- `failoverbench --mode all` passes with the updated `attempts` expectation (see Implementation 4).

---

## R2 — Replace validated claim interpolation with `set_config` binds (`wamn_postgres.rs`)

### Problem
`begin_with_claims` (`wamn_postgres.rs:599`) builds `BEGIN; SET LOCAL app.tenant = '{tenant}'; SET LOCAL statement_timeout = {ms};` (+ optional ` SET LOCAL search_path = {schema};`) via `format!`, guarded by charset validation (`valid_tenant`/`valid_schema`, `[A-Za-z0-9_-]{1,64}` / no-hyphen variant). This is *safe today* — inputs are host-derived and validated — but it contradicts the platform invariant the WIT itself states ("there is no interpolation path"), and the `format!` template is the foot-gun: a future claim (e.g. `app.user`, `app.role` arriving with 4.2) can be appended by someone who forgets the validator, and nothing structural stops them. House principle: injection should be *unrepresentable*, not *validated*.

### Design
`SET LOCAL` cannot take bind parameters, but its exact equivalent can: `set_config(name, value, is_local := true)`. Replace the interpolated claim segment with one prepared, fully-bound statement executed after `BEGIN`:

```sql
SELECT set_config('app.tenant', $1, true),
       set_config('statement_timeout', $2, true),
       set_config('search_path', COALESCE($3, current_setting('search_path')), true)
```

- `statement_timeout` binds as text (GUC values are strings; `set_config('statement_timeout','5000',true)` ≡ `SET LOCAL statement_timeout = 5000`).
- `search_path` binds the schema name as a value, closing the identifier-embedding path; `$3 = NULL` preserves today's "absent leaves server default" behavior via the `COALESCE`. (If the self-referential `current_setting` read proves awkward, the fallback is two prepared variants — with/without the search_path column — still zero interpolation.)
- **Keep** `valid_tenant`/`valid_schema` as *identity-format* checks (they define what a legal tenant/schema id is, and the no-hyphen schema rule still matters for DDL-side quoting); they are no longer the security boundary.

### Cost & the honest trade-off
Today the claims ride the same `batch_execute` round trip as `BEGIN` (simple query protocol — which is *why* they're interpolated; simple protocol can't bind). The fix makes it `BEGIN` + one prepared statement = **one extra round trip** on transaction open. tokio-postgres pipelining (join the two futures) can collapse most of that. S2 measured p50 710 µs / p99 1.98 ms with 5–10× gate headroom; an added ~100–200 µs in-cluster RTT is well inside it, but it must be *measured*, not assumed.

### Implementation
1. Rewrite `begin_with_claims`: `batch_execute("BEGIN")` → prepared `set_config` statement (cached host-side like all statements) with pipelined execution where the driver allows.
2. Delete the `format!` claim template; the function signature and every caller (one-shots, explicit `transaction`, cursors) are unchanged.
3. Grep-gate: no remaining `format!` containing `SET LOCAL` in the plugin (the janitor/queue SQL uses binds already; `current_setting` reads are unaffected).

### Verification
- Re-run the full S2 gate set (throughput, p99, chaos ×100, RLS ×10k, injection ×10k) — thresholds unchanged; record the new p50/p99 next to the old in `docs/p0-results.md` as an addendum so the round-trip cost is on the record.
- New injection cases targeting the claim path itself: tenant/schema strings containing `'`, `;`, `--`, unicode — now legal to *attempt* (they bind as data and simply name a nonexistent tenant/schema) where before they were rejected pre-SQL; assert no statement-level effect either way.

### Doc closure
`docs/security-db-path.md` + the WIT header comment: the "no interpolation path" claim becomes unconditionally true; note the identity-format validators' demoted role.

---

## R3 — Per-component memory limits via `ResourceLimiter` (fork commit #2)

### Problem
S1 finding #2: upstream carries `LocalResources.memory_limit_mb` end-to-end but never plumbs it into wasmtime — no `Store::limiter` call sites exist. The current 256 MiB cap is the pooling allocator's **engine-wide** `max_memory_size`: uniform across all components on a host, not per-workload. The plan promises per-component sandbox caps (1.3, 8.2's isolation story, 9.8's "memory vs cap" metric); that promise is currently weaker than written, and unlike the epoch gap it has no decision record.

### Design
**Two-tier semantics.** The pooling allocator's `max_memory_size` is the **platform ceiling** (largest budget any component may hold; pre-sizes pool slots). `memory_limit_mb` is the **per-component budget** enforced *below* the ceiling by a per-`Store` `ResourceLimiter` — pure arithmetic (`desired > limit → deny`), consulted by wasmtime on initial memory creation and every `memory.grow`. Denial makes `grow` fail in-guest → allocator abort → service trap: the exact failure shape S1 already validated at the engine cap, so restart accounting is untouched. Initial-memory checking is a freebie: a component whose baseline exceeds its budget fails at instantiation with a clear error rather than at first growth.

**Strictness rules:** budget > ceiling is a hard workload-start error (never a silent clamp). Absent field ⇒ budget = ceiling ⇒ behaviorally invisible to every existing workload (the regression guard). v0 enforces per-linear-memory with a documented note on multi-memory components (overwhelmingly single-memory today; store-wide cumulative accounting is a follow-up if the builder ever emits multi-memory artifacts). The same limiter applies a modest fixed table-growth cap (closes the resource-exhaustion sibling for free).

### Implementation (fork commit #2 on `wamn/2.5.2`)
Same file as the epoch commit — `new_store_from_templates` in `crates/wash-runtime/src/engine/linked_call.rs`, the single production store-creation site: construct the limiter from the resolved budget and attach via `Store::limiter`. Budget resolution order: `LocalResources.memory_limit_mb` if the workload spec is reachable at that site; else the `wamn.memory-limit-mb` component-config key (the identical stopgap routing the epoch commit uses); else `WAMN_MEMORY_LIMIT_MB` env; else ceiling. Verify the sync `ResourceLimiter` trait suffices with the async store (expected; `ResourceLimiterAsync` is the fallback if wasmtime demands it). Commit message carries the ledger fields; exit condition: *"upstream plumbs memory_limit_mb into a Store limiter — delete this commit."* This is the most upstreamable change we carry (it implements their own dead field); if the no-upstream-issues stance ever softens, this is the one to file.

### Observability — the limiter is also the meter
9.8's "per-component memory vs cap" metric is born here: record per-component high-water mark; increment `wamn.memory.denied{component}` on every refusal (S5's counted-never-silent discipline). Denials become an alertable signal and high-water data is what lets operators right-size tier budgets.

### Verification (new `wamn-host bench` phase, S1-style)
1. **Cap honored:** `memhog` at a 64 MiB budget under a 256 MiB ceiling traps at ~64 MiB; heartbeat healthy; host accepts new work.
2. **Differentiation (the point):** two components, 64 and 192 MiB budgets, resident concurrently, each capped at its own number — the gate that closes S1 finding #2.
3. **Denial observability:** metric incremented with component id; log line present.
4. **No-regression:** unlimited workloads byte-identical to today; S1 instantiation p50/p99 unchanged (one predicate per grow — expect noise, confirm).

### Doc closure
Decision-table row (chosen: ceiling + per-component limiter; rejected: engine-uniform — contradicts 8.2; wait-for-upstream — dead field, no signal). Amend "per-component sandbox limit" claims to say **linear-memory** budget (compiled-code residency, stacks, host-side buffers live under separate engine config — 8.2 must not overclaim). Mark S1 finding #2 resolved with commit + bench-phase pointers.

---

## R4 — Fork-based upstream management: Cargo.toml switch + sync runbook

Fork state: `wamn/2.5.2` branch = upstream 2.5.2 rev + the epoch commit (R3 adds commit #2). Patch 0002 (workspace lints) is expected dead: it existed because `[patch]` vendoring made wash-runtime a *path* dep, and path deps don't get the `--cap-lints allow` that git deps get; consumed from the fork as a git dep, upstream lints are capped automatically. Confirm at the rebuild below; only re-add (as a fork commit) if `-D warnings` actually fires.

### 4a. Cargo.toml switch (one-time)

In the **root** `Cargo.toml`:

```toml
# [workspace.dependencies] — replace the upstream pin (line ~20):
wash-runtime = { git = "https://github.com/dkkloimwieder/wasmcloud", rev = "<TIP_SHA_OF_wamn/2.5.2>", default-features = false, features = ["washlet", "wasi-config", "wasi-logging", "wasi-otel"] }

# DELETE the entire [patch."https://github.com/wasmCloud/wasmCloud.git"] block (~line 31–35)
```

Rules: pin a **rev** (immutable), never a branch name (moves); the branch's existence on the fork is what keeps the SHA fetchable. `crates/wamn-host/Cargo.toml` keeps `wash-runtime = { workspace = true }` untouched. **wasmtime alignment:** wamn-host pins wasmtime to the exact line wash-runtime's workspace uses (so the graph carries one wasmtime) — 2.5.2 may have bumped it; read `vendor`-era notes vs. the fork's workspace `Cargo.toml` and align wamn's wasmtime pin in the same change.

Then delete the apparatus: `scripts/vendor-wasmcloud.sh`, `patches/` (+ `patches/README.md`), the `/vendor/` line in `.gitignore`, and the Dockerfile's vendor stage (`COPY`/`RUN ./scripts/vendor-wasmcloud.sh` → nothing; the git dep resolves inside `cargo build`, which needs network in the build stage — already true for crates.io).

Validate: `cargo update -p wash-runtime` (lockfile now records the fork source+rev) → clean build (patch-0002 verdict lands here) → run the **upgrade gate subset** (below) once to establish the fork baseline → commit as `chore(1.7): consume wash-runtime from fork wamn/2.5.2; delete vendoring`.

### 4b. Standing sync runbook

**Triggers, in priority order:** (1) wasmtime **security advisory** touching our version line — immediate (`cargo audit` run weekly against the lockfile until CI exists); (2) upstream **minor release** — evaluate, don't chase; batch quarterly unless a needed fix/feature pulls; (3) the **WASI 0.3 / Wasmtime major** milestone — planned work, coordinate with the `wamn:node` 0.2 contract revision.

**Steps per sync (upstream releases X.Y.Z at rev `NEWREV`):**
```bash
# in the fork clone (remote 'upstream' = wasmCloud/wasmCloud)
git fetch upstream
# 0. pre-read: what moved in the files we carry commits against?
git log --oneline <OLD_BASE>..NEWREV -- crates/wash-runtime/src/engine/
# 1. new branch from the new upstream point (fork main stays untouched/stale — irrelevant)
git checkout -b wamn/X.Y.Z NEWREV
# 2. carry the wamn commits forward
git cherry-pick <epoch-commit> <limiter-commit>      # or: git rebase --onto NEWREV <OLD_BASE> wamn/OLD
# 3. conflict = review event, not merge chore: upstream changed that code for a reason — read it.
#    Also re-check each commit's EXIT CONDITION: if upstream now does it, DROP the commit.
git push -u origin wamn/X.Y.Z
```
Then in wamn: bump the `rev` to the new branch tip, re-align the wasmtime pin to the new workspace's line, `cargo update -p wash-runtime`, rebuild, and run the **upgrade gate subset** — deliberately not all of P0, just the fork-load-bearing behaviors:
- **S1:** instantiation p50/p99 + cap-kill + the epoch-deadline demo (`wamn-host bench`),
- **S2:** the chaos gate (epoch-kill mid-transaction ×100; destroy-never-repool) (`pgbench`),
- **S3:** kill/resume idempotency (`flowbench`),
- **R3's phase** once it exists (differentiation gate).

Record in the fork branch's final commit message (or `docs/p0-results.md` addendum): date, base rev old→new, commits carried/dropped, gate numbers old→new. Budget: an afternoon when clean.

**Rollback:** the old `wamn/OLD` branch is never deleted — repoint wamn's `rev` at its tip and rebuild. Retention: keep every branch wamn ever pinned (they're cheap and they're the bisect trail).

**Drift check between syncs** (before any host-touching feature work): `git fetch upstream && git log --oneline <BASE>..upstream/main -- crates/wash-runtime/src/engine/` — a heads-up that the carried code moved, before an advisory puts a clock on the upgrade.

**Escalation threshold (standing):** the fork carries **host-integration commits only** (things upstream should arguably own); wamn features never land there. If the fork grows past ~4–5 commits or the same commit conflicts on consecutive syncs, that is no longer dependency management — engage upstream or explicitly accept runtime-maintainer status as a decision-table entry.

---

## R5 — RLS claim-shape hardening + scope honesty

### 5a. `NULLIF` the claim read in policy templates (`wamn-rls`)
Policies compare `tenant_id = current_setting('app.tenant', true)`. Postgres resets a custom GUC to the **empty string** (not NULL) after `SET LOCAL` scope ends, so an idle pooled connection carries `''` — which matches nothing *only while no row ever has an empty `tenant_id`*. That is accidentally-match-nothing, not structurally-match-nothing. Fix in the single quoting/template source (`wamn-rls/src/compile.rs`): emit `tenant_id = NULLIF(current_setting('app.tenant', true), '')` — `NULL` comparison matches no row, ever, including a hypothetical empty-tenant row. Belt-and-braces: `wamn-catalog` validation + `wamn-seed` reject empty/whitespace `tenant_id` values, and the system-schema DDL adds `CHECK (tenant_id <> '')` on tenant-scoped tables. Verification: extend the S2 RLS gate with an *empty-claim* pass — a connection with claims deliberately unset queries a table seeded (superuser fixture) with an empty-tenant row; assert zero rows both before and after the template change (documenting that the fix converts an invariant-dependent pass into a structural one).

### 5b. Scope honesty: S2 proved tenant-level RLS only
The S2 pass is component-identity → `app.tenant`. User/role-level enforcement (`app.user`, `app.role`, field-level masks) does not exist yet — it arrives with 4.2/4.3 — and the S2 result must not be over-read as "RLS validated" generally. Actions: a one-line scope note in `docs/p0-results.md` S2; and 4.2's acceptance criteria gain an S2-style randomized gate at the *user* level (two users, one tenant, row-ownership policies, ×10k cross-user attempts, zero leaks; field-mask read/write assertions for 4.3). The R2 `set_config` statement is where `app.user`/`app.role` will bind when they arrive — bound parameters from day one, never joining a `format!` template (the R2 rationale in action).

---

## R6 — `partitioned(key)` ordering under retry/park: decide the policy, don't inherit it from the SQL

### Problem
Commit `75d8277` delivers per-partition ownership with head-first, one-in-flight dispatch keyed on `(available_at, run_id)`. The head-blocking `NOT EXISTS` counts only *ready* earlier runs — so a run that fails transiently and backs off (future `available_at`), or parks on a delay node, **yields the key**: the next run of the partition becomes the head and dispatches first. `docs/run-queue.md` explicitly defers the *terminal*-failure case (wedge vs release) to 5.11 — correctly — but the *transient* case is silently decided by the mechanism: `partitioned(key)` currently means **ordered-except-under-retry-or-park**.

That is weaker than the platform's stated semantics (plan 5.11: "order preserved per key — the Kafka model"; a Kafka consumer blocks its partition on retry, it never leapfrogs) and weaker than what the target workloads require: genealogy/traceability streams (consume-before-produce, state-machine transitions per asset) corrupt under exactly this reordering, triggered by nothing more than a transient network blip. The danger is not the current behavior per se — it is that flows will ship and silently depend on whichever behavior exists, after which changing the default is a breaking change.

### Design decision required (5.11, before any partitioned flow ships)
Make ordering-under-unavailability an explicit per-flow (or per-node) policy on `partitioned(key)`:

- **`blocking` (proposed default):** a backed-off or parked run still blocks its key — the partition waits out the backoff/park and retries the head in place. Strict per-key order, at the cost of head-of-line latency. Rationale for default: choosing `partitioned` *is* opting into ordering; paying latency for it is the expected contract, and it matches the Kafka mental model the plan invokes. Mechanically cheap: the head-blocking `NOT EXISTS` additionally treats an earlier not-yet-ready, not-exhausted run of the same key as blocking (drop the `available_at <= now()` arm for the blocker check), i.e. the head is the earliest *live* run, ready or not.
- **`leapfrog` (opt-in):** today's behavior — order is `(available_at, run_id)`; backoff/park re-enters at its new position. Correct for keys where ordering is a throughput heuristic rather than a causal requirement (per-key fairness, cache locality).
- The already-deferred **terminal wedge vs release** question folds into the same knob: `blocking` + budget exhausted ⇒ wedge (operator unblocks — the janitor's `infrastructure-failure` verdict releases the key only under `leapfrog`); this is the strict-traceability behavior D14's genealogy module will want.

### Implementation sketch
`wamn-flow` ordering config gains the policy field (default `blocking`); `claim_partition_head_sql` branches its blocker predicate on the key's policy (join to the flow's ordering config, or materialize the policy onto the queue row at enqueue — the latter keeps the claim SQL self-contained and is preferred); `partition.rs` pure layer models both; the park path needs no change (parking already releases the run lease — under `blocking` the parked run simply remains the head).

### Verification
`queuebench` partition phase gains both-policy cases: under `blocking`, kill/backoff the head and assert run 2 does **not** dispatch until the head completes; under `leapfrog`, assert today's behavior. A wedge case: exhaust the head's budget under `blocking`, assert the key stalls and the janitor verdict does not release it; operator-release path asserted once 5.11 ships the intervention surface.

### Doc closure
5.11's semantics table gains the policy column; `docs/run-queue.md`'s "policy decision that belongs to 5.11" paragraph is replaced by a pointer to the decided policy; decision table gains the row (chosen: per-key policy, `blocking` default; rejected: inherit `(available_at, run_id)` order silently — semantics by accident).

---

## R7 — Two operational notes from the failover commit (`a7d2ad2`)

**7a. Status-flip false alarms.** The reverse-race resolution is correct (janitor reaps a slow-but-alive resume → `infrastructure-failure`; the guest's deliberately unconditional completion write overrides → `completed`; the work happened exactly once and the final status says so). But any alert that fires on the `infrastructure-failure` *transition* will false-alarm on every slow resume. 9.10 alerting rule: fire on `infrastructure-failure` **sustained** past a delay (or on janitor verdicts not subsequently overridden within it), not on the transition. And the janitor grace period should be sized against worst-case reconstruction time — which `failoverbench` can now measure; record that number and derive the grace from it rather than guessing.

**7b. Failover latency is two-lease.** A new owner acquires a dead replica's partition immediately after the *partition* lease expires, but the dead runner's in-flight run still blocks the head until the *run* lease expires — effective partition failover = `max(partition-lease TTL, run-lease TTL)`. One line in `docs/run-queue.md` so nobody tunes one TTL and wonders why failover didn't speed up.

---

## R8 — Dispatcher tranche findings (`a3fb0b3`, `b687d45`)

Context: the dispatcher achieves exactly-once **leaderless** — deterministic fire identities (`{flow}:cron:{tick13}` from the second-truncated scheduled tick; `{flow}:outbox:{seq}`) + `ON CONFLICT` write-ahead absorb racing replicas, and poll→fire→ack co-transacted means a crash redelivers AND retracts atomically. These findings are edges of that design, not flaws in it.

### R8a — Cron anchor vs run-history retention (latent duplicate fire)
**Problem.** The dispatcher's cron state *is* the runs table: `cron_last_run_sql` recovers the last-fired tick from the flow's own cron runs — elegant, nothing dispatcher-local to desync. But run history will be retention-pruned (9.6 tiers). If retention < cron period (e.g. 7-day retention, monthly job), the anchor is pruned between fires; `due_tick` then computes with no anchor and re-fires an already-fired tick — and the `ON CONFLICT` guard cannot absorb it because the conflicting row is the one that was pruned. Duplicate fire of a (by construction) infrequent, likely-important job.
**Design options.** (a) Retention exemption: the pruner always keeps each flow's latest `{flow}:cron:*` run (max run per flow, cron kind) — no new state, retention logic gets one carve-out. (b) Anchor table: tiny `cron_anchor(flow_id, last_tick)` upserted inside the fire transaction — explicit state, immune to any retention policy, one more row per fire. (c) Validation rule (retention ≥ 2× longest cron period) — rejected: couples two policies that will be tuned by different people at different times; fails silently when either moves.
**Recommendation:** (b) — the fire transaction already exists, the anchor is then *definitionally* correct rather than correct-by-retention-policy, and `cron_last_run_sql` becomes a fallback for anchor bootstrap. Decide and implement **before 9.6 retention ships**; until then it is latent by construction (nothing prunes yet).
**Verification.** dispatchbench gains a retention mode: fire a tick, delete the run row (simulated prune), advance stepped time past the next tick, assert exactly the *next* tick fires (no re-fire of the pruned one).

### R8b — Dispatcher DB role scoping
**Problem.** The projects Secret centralizes every project's DB credentials in one always-on control-plane deployment — inherent to the dispatcher's job (it must reach all projects), same blast-radius logic as the D2 per-project-gateway argument. But `deploy/run-queue.sql` grants the `wamn_run` tables to `wamn_app` — the runtime role that also holds user-entity grants. If the dispatcher connects as `wamn_app`, a compromised dispatcher reads *user data* it never needs: its entire job is `run_queue` / `runs` / `outbox` / trigger registry, and row-event payloads come from the outbox rows, not the entity tables.
**Fix.** Dedicated per-project `wamn_dispatch` role: `GRANT` on the `wamn_run` schema tables only (queue, runs, outbox, registry, partition_owner as needed), **zero** user-schema grants, same `FORCE RLS` tenant floor. The projects Secret carries `wamn_dispatch` credentials. Compromise then yields trigger forgery/replay (bounded, auditable — deterministic run ids make replays visible) instead of tenant-data read. The NATS analogue (allow-all publish user → publish-only `verify_and_map` user) is already flagged in `deploy/dispatcher.yaml`; land both scopings together.
**Verification.** A dispatchbench (or S2-style) negative gate: the `wamn_dispatch` connection attempts a user-entity `SELECT`, asserts `permission-denied`.

### R8c — Outbox write-amplification + GC verification
**Problem (amplification).** `b687d45` emits `AFTER … FOR EACH ROW` triggers uniformly across **all** entity tables when the (opt-in) plan is applied. A 100k-row bulk import pays 100k outbox inserts inside the user's transaction (write amplification, txn bloat, WAL) and then up to 100k firings per registered flow. The doc's "dispatcher acks unregistered rows cheaply" is true but the *write* cost is paid regardless of registration.
**Fix direction.** Per-entity emission driven by actual row-event flow registration (or an explicit designer flag per entity), reconciled when registrations change — the trigger-emission plan already supports idempotent re-apply (`CREATE OR REPLACE`, constant-named triggers), so narrowing its coverage is mechanical. Separately: a coalescing/rate policy story for registered high-churn tables (statement-level triggers with transition tables are the escape hatch if per-row ever dominates — payload shape change, so a deliberate decision, not a default). Gate this **before bulk-import tooling (3.6)** lands.
**Problem (GC — verify).** Acked rows set `dispatched_at` and remain. Confirm a pruner exists for dispatched outbox rows (janitor sweep or DDL-side policy); if not, the outbox grows without bound on every project. If missing: co-locate pruning with the janitor sweep (`DELETE … WHERE dispatched_at < now() - interval`), interval generous enough for forensics.
**Verification.** Bulk-write bench case (10k-row single-statement UPDATE on a registered table): outbox insert cost measured, firing count correct, and post-ack prune observed.

### R8d — Cron misfire collapse: document the contract
**Problem.** Dispatcher downtime spanning multiple ticks fires only the **latest** (misfire collapse). Right default — but jobs whose ticks denote *work items* (hourly aggregation windows) silently lose windows, and nothing tells a flow author which contract they have.
**Action now:** document collapse as the cron contract (flow-editor trigger docs + `docs/run-queue.md`). **Later (5.11-adjacent backlog):** per-flow misfire policy (`collapse` default | `catch-up` with a bounded backfill window), Quartz-precedented. Deterministic tick-named run ids make `catch-up` mechanically trivial when wanted — fire each missed tick's id; `ON CONFLICT` dedupes.
**Linkage (no action, R6 evidence):** row-event flows on genealogy tables will want `partition_key = <row pk>` so outbox `seq` order survives dispatch — one more consumer for R6's `blocking` default.

---

## R9 — Migration-ordering commit findings (`873c3d8`)

Context: the name-freeing preamble is correct, well-gated (hoisted destructive ops keep their destructive classification, so confirmation gating is not bypassed by reordering), and regression-contained (collision-free plans byte-unchanged). These findings are consequences at its edges. Two pre-existing defects surfaced by the same adversarial review are already filed in-repo — `wamn-drb` (FK-on-retype) and `wamn-nqg` (removed-tables drop order) — referenced here for traceability, not re-derived.

### R9a — Reserve the `wamn_` identifier prefix at catalog validation
**Problem.** The compiler's `TempNameCollision` check conservatively rejects a plan whose synthesized aside name (`wamn_mig_drop_<table>`) collides with any relation in either catalog version — correct, but it means a user who names an entity `wamn_mig_drop_orders` learns it at *migration-compile* time, and only when a colliding evolution happens to occur.
**Fix.** Catalog validation (3.1) rejects entity / index / constraint names beginning `wamn_` outright: the designer fails at design time with a clear rule; the entire prefix stays free for present and future system machinery (migration asides, outbox artifacts, run-schema objects); the compile-time collision check demotes to defense-in-depth. One validation rule + one test; document the reserved prefix in the schema-designer naming rules.

### R9b — Table rename × row-event registration: silent trigger loss
**Problem.** Outbox triggers are rename-safe (constant-named, `CREATE OR REPLACE` — a renamed table keeps its trigger) and emit `TG_TABLE_NAME` — which after a rename is the **new** physical name. Row-event flows register on the physical table name. Renaming a watched table therefore makes every registered row-event flow **silently stop firing**: no error, no failed run, no signal — the exact silent-breakage class 11.8's schema-impact analysis exists to catch, currently uncovered.
**Fix.** (1) Name the case in 11.8's spec and test set: staging a migration that renames a table flags every row-event flow registered on it. (2) Decide the remediation policy — auto-migrate the registration to the new name in the same catalog bump (matches the trigger's rename-follows behavior; recommended) vs. require explicit re-registration (safer against accidental renames, worse UX). (3) Interim, before 11.8 exists: a warning line in the DDL-compiler and flow-trigger docs, and — cheap and worth doing — the migration path emits a notice when a renamed table has registered row-event flows.
**Verification.** dispatchbench (or a ddl live-apply case): register a row-event flow, rename the table, write a row — assert the chosen policy's outcome (auto-migrated registration fires; or the staged plan is blocked/flagged) rather than silence.

### R9c — The one-transaction apply assumption now carries more weight: record its expiry
**Problem.** The preamble's aside-renames leave zero residue *because* apply is one transaction — rollback undoes them. That assumption has a known future breaker: `CREATE INDEX CONCURRENTLY` (which zero-downtime index builds on large tables will eventually demand) cannot run inside a transaction block. The moment 3.2 grows a non-transactional migration step, a mid-apply crash can orphan `wamn_mig_drop_*` relations (and half-built indexes), and nothing today would clean them up.
**Action now:** one paragraph in `docs/ddl-compiler.md` stating the one-txn invariant as load-bearing for the preamble and naming CONCURRENTLY as its known expiry. **Then (with the future non-txn work, not before):** a residue janitor — startup/scheduled sweep dropping `wamn_mig_drop_*` relations older than a grace period with no in-flight migration — plus the apply-journal machinery non-txn migrations need anyway. This is a standing constraint to inherit knowingly, not current work.

---

## Sequencing

1. **R4a** — Cargo.toml switch + apparatus deletion + baseline gates (immediate; everything else lands on top).
2. **R1** — the correctness bug, now spanning both claim paths + the failoverbench expectation; small diff, gates P1's production runner work.
3. **R6** — the ordering-policy decision; must be *decided* before any partitioned flow ships, implemented with the 5.11 semantics work (natural pairing with R1's partition-path changes — same files, one review).
4. **R3** — fork commit #2 + bench phase, before P1 multi-tenant density work.
5. **R2** — with the 2.2 production-plugin issue (re-runs S2 gates anyway); mandatory before 4.2 adds user/role claims.
6. **R5a** — with the next `wamn-rls` touch; **R5b** rides 4.2. **R7a** rides 9.10 alerting; **R7b** is a one-line doc touch, anytime.
7. **R8b** (dispatch role) + the NATS `verify_and_map` scoping — next dispatcher/deploy touch, one commit. **R8c-GC** — verify now (five-minute check), fix with the janitor if missing. **R8a** — decide now, implement before 9.6 retention exists. **R8c-amplification** — before 3.6 bulk import. **R8d** — doc line now, knob later.
8. **R9a** — one validation rule, next 3.1 touch. **R9b** — interim doc warning + migration notice now; the full case lands in 11.8's spec (its remediation policy — auto-migrate vs re-register — is a small decision to take when filing). **R9c** — one doc paragraph now; the residue janitor is deferred to the non-txn migration work it belongs to. `wamn-drb` / `wamn-nqg` remain tracked in-repo.

Also bankable now: the queuebench regression numbers from `a7d2ad2` (write-ahead p99 1.11 ms, fast-path 361 µs, doorbell 300/300) sit ~13×/~27× under the proposed D15 SLOs — the "proposed, pending sign-off" flag on D15 can close with data.

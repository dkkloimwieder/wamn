# Structure & Code-Quality Review — SR1–SR7

Scope: workspace structure, code organization, coherence, and conventions at tip `e300859` (16 crates, 11 components, ~37k lines). This is the "while the project is still new" review: every finding is cheapest now and compounds if deferred. Companion to `review-findings.md` (R-series = correctness/design findings; SR-series = structure/quality). Guideline citations reference the house Rust guidelines (M-rules).

**Priority summary:** SR1 and SR2 are the substantive refactors — do them before the next gate/flow lands, respectively. SR3 is a layout decision to take once. SR4–SR6 are ride-alongs on files already being touched. SR7 is a conventions write-down, ~an hour.

| # | Finding | Effort | When |
|---|---|---|---|
| SR1 | Split gates out of `wamn-host` (lib-ify host; `wamn-gates` binary; two image targets, one Dockerfile) | ~1–2 days | Before the next bench lands |
| SR2 | De-duplicate run-state SQL: `flowrunner` re-implements `wamn-run-store` in guest SQL | ~1 day | Before F3/F4 flows |
| SR3 | Repo tiering: `poc/` top-level; `components/{fixtures,samples}/`; `poc-` component prefix | ~half day | Once, now |
| SR4 | `wamn_postgres.rs` internal module split | ~half day | With the next plugin touch (R2 is queued) |
| SR5 | `CronError(String)` → structured variants | ~1 hour | With the next dispatcher touch |
| SR6 | Write down the conventions that are currently implicit | ~1 hour | Now (AGENTS.md/CLAUDE.md) |
| SR7 | WIT vendoring: single source, path-shared | Optional | Only if the coherence test ever fires |

---

## SR1 — `wamn-host` is two programs in one crate: split the gates out

### Problem
Of `wamn-host`'s ~14,150 lines across 25 modules, roughly 9k are gate tooling: eleven `*bench.rs` modules (`bench`, `pgbench`, `flowbench`, `queuebench`, `dispatchbench`, `failoverbench`, `nodebench`, `logbench`, `egressbench`, `apibench`, `f1bench`, `testhostbench`) plus the `apifixture`/`apiproof`/`f1fixture`/`f1proof` quads. The production host — `engine`, `host`, `dispatch`, `plugins/` (3), `publish_catalog` — is ~5k lines sharing one binary with its own verification harness.

Consequences, in order of weight:
1. **The production washlet image ships the gates.** `f1fixture` seeds databases; proof modes drive assertions; `logspewer`-driving and chaos-kill code is present in the binary running untrusted tenant components. For a platform whose pitch includes isolation discipline, the prod artifact should not contain its own attack tooling (M-TEST-UTIL: test utilities are feature-gated — here, binary-gated).
2. **Compile-time coupling.** Every gate edit rebuilds the host; every host edit rebuilds eleven benches. With no CI, iteration speed *is* the test-run budget.
3. **Crate imbalance** (M-SMALLER-CRATES, M-BALANCED-MODULES): the crate's public shape no longer states what it is. Eleven benches accreted in ~3 weeks; this only grows.
4. **Utility duplication is a symptom:** `percentile` exists 4× (bench.rs, pgbench.rs, queuebench.rs, flowrunner); env-parsing and PG-connect helpers repeat across bench files. Duplication happens because there is no shared harness to put them in.

### Recommendation
Three crates + one Dockerfile with two final stages ("separate gates image" produced by **one build invocation** — minimal prod artifact without doubling the build workflow):

1. **`wamn-host` becomes lib + thin bin.** Move `engine`, `host`, `dispatch`, `plugins/`, `publish_catalog` under `src/lib.rs` (public API: what gates need to construct engines/hosts/plugins); `src/main.rs` shrinks to CLI parsing + the production subcommands (washlet run, publish-catalog). Nothing about the production code changes except visibility (`pub(crate)` → `pub` where gates consume it — audit each promotion; the lib boundary is a chance to notice what gates reach into that they shouldn't).
2. **`wamn-gate-harness` (new lib crate):** the shared measurement/assertion vocabulary — percentile/stats, env-var parsing (one place to enforce the `WAMN_*` naming convention), PG connect/seed helpers, gate assert/report formatting, the stepped-clock helpers dispatchbench uses. This is where the 4× `percentile` collapses to 1× (flowrunner's copy goes with SR2's slimming; guest can't depend on a native harness — its one call site moves host-side or into the run-state SQL return shape).
3. **`wamn-gates` (new bin crate):** the eleven bench modules + fixture/proof quads, depending on `wamn-host` (lib) + `wamn-gate-harness`. Subcommand surface identical to today (`wamn-gates pgbench …`), so `deploy/*-job.yaml` changes are `command:` swaps only.

**Dockerfile:** one build stage compiles both binaries; two final stages — `wamn-host` (prod: host binary only) and `wamn-gates` (FROM the prod stage + gates binary, so gates Jobs still exercise the *identical* host lib code they verify). Two tags from one `docker build` invocation (`--target`).

### Implementation steps
1. `git mv` bench/fixture/proof modules to `crates/wamn-gates/src/`; mechanical `mod`/`use` fixes.
2. Create `src/lib.rs` in wamn-host; promote the items gates consume (expect: `Engine`/host builders, plugin constructors, config structs). Record anything that required promoting *internal* host state — those are boundary smells to fix, not promote.
3. Extract harness fns into `wamn-gate-harness`; delete the duplicates; `cargo tree -i` to confirm no gate-only deps (e.g. stats/CSV crates, if any) remain in wamn-host.
4. Dockerfile: `--target host` / `--target gates`; update `deploy/*.yaml` images + commands.
5. **Verification:** prod image size recorded before/after (expect meaningful shrink); `strings` spot-check that fixture SQL is absent from the prod binary; full gate suite re-run from the gates image against a washlet from the prod image — proving the split didn't fork behavior.

### Rejected alternatives
*Feature flag, one binary:* prod must then be built with default-off and nothing enforces it (no CI); accidental gates-on ships silently; test matrix doubles. *Second binary, same image:* one tag, but the attack tooling still ships to prod — fails consequence #1, which is the point. *Do nothing until CI exists:* the split is what makes an eventual CI cheap (prod build ≠ gates build is the natural pipeline boundary).

---

## SR2 — `flowrunner` re-implements run-state persistence in guest SQL

### Problem
`components/flowrunner/src/lib.rs` (1,126 lines, single file) hand-writes `open_run`, `record_node_run`, `mark_completed`, `load_wake`, `save_wake`, `load_completed` — inline SQL against the same `wamn_run` tables whose shapes `wamn-run-store`/`wamn-run-queue` own host-side. Two authors of one schema's SQL; the drift guard is indirect (reconstruct/failover gates catch *behavioral* divergence, not e.g. a column silently defaulted). The component also carries a hand-rolled HTTP URL parser + client, WIT↔SDK conversions, flow-JSON fixtures, and the fourth `percentile` copy. A component shell should be thin dispatch glue; this one is accreting a platform.

The repo already invented the correct pattern and uses it five times: **pure SQL-text builders in a crate, executed by whoever holds the connection** (`wamn-f1::sql` states it explicitly: values always `$n`, identifiers pinned). Run-state SQL is the one schema not yet under that regime.

### Recommendation
1. **Single source for run-state SQL:** ensure `wamn-run-store`'s SQL lives in a pure, guest-compilable module (`wamn_run_store::sql` — text builders + param marshaling only; no tokio/deadpool in that module's dependency closure). If crate-level deps prevent wasm32 compilation, split `wamn-run-sql` out (M-SMALLER-CRATES cuts both ways — prefer the module if the crate can be made target-clean, the split only if it can't).
2. **flowrunner consumes it** for all seven persistence functions; the inline SQL is deleted. Host-side callers likewise consume the same builders (they may already — verify; if not, converge them in the same change).
3. **Slim the rest of the shell:** WIT↔SDK conversions (`sdk_to_wit`/`wit_to_sdk`, `GuestCtx`) are the capability-bearing twin of what `wamn-node-guest` does for the no-caps world — move them to a `wamn-node-guest` module (or sibling) so the *next* capability-bearing component doesn't copy them. The hand-rolled HTTP client: replace with the SDK/`wamn-nodes` http path if reachable, else isolate in its own module with a TODO naming its replacement (5.10-era). Bench-ish helpers (`percentile_ns`, flow-JSON fixtures) move to the gates side (SR1) — a production component must not contain its own bench fixtures.
4. **Target:** flowrunner ≤ ~400 lines of dispatch glue, and a stated house rule (SR6): *components are shells; logic lives in crates.*

### Verification
Reconstruct/failover/flowbench gates green after the swap (same behavior, one SQL source); `grep -rn "INSERT INTO\|UPDATE " components/` returns only builder calls, no inline run-state SQL; wasm32 build of the shared module proven in the components workspace.

---

## SR3 — Repo tiering: make the platform/POC/fixture boundaries visible in the tree

### Problem
`crates/` mixes three tiers with no signal: platform infrastructure (11 crates), the node ecosystem (4), and POC consumer logic (`wamn-f1`). `components/` mixes production components (`flowrunner`, `api-gateway`, `webhook-entry`, `flow-driver`) with bench fixtures (`busyloop`, `memhog`, `logspewer`, `hello`, `pgprobe`) and samples (`sample-node`, `node-rs`, `node-ts`). The plan's own "the POC is a consumer, not a roadmap" boundary — and the prod-vs-fixture boundary SR1 enforces at the binary level — are invisible in the filesystem. At 27 units it's navigable; F3/F4 and the next benches make it worse.

### Recommendation (decided: option b — split by nature, prefix components)
1. **`poc/` top-level directory** for native POC crates: `git mv crates/wamn-f1 poc/f1` (crate name can stay `wamn-f1`; workspace `members` gains `"poc/*"` or the explicit path — M-CRATES-FLAT-FOLDER is satisfied per-tier). F3/F4 pure-logic crates land there. Rationale for not making `poc/` its own workspace: one lockfile, one `cargo test` sweep, and POC crates deliberately exercise platform crates — a separate workspace would hide breakage instead of surfacing it.
2. **POC wasm components stay in `components/`** (they must live in the wasm32 workspace) **with a `poc-` prefix:** rename `webhook-entry` → `poc-webhook-f1` *now* while the deploy-manifest churn is two files; new POC components are born prefixed.
3. **`components/fixtures/` and `components/samples/` subdirectories** for the bench fixtures and the sample nodes (workspace member paths update; nothing else changes). Production components remain at `components/` root — the root *is* the production tier.

### Verification
`cargo build --workspace` + components-workspace build green; deploy manifests updated (grep for the old component name); one paragraph in AGENTS.md stating the tiering rule so it self-maintains.

---

## SR4 — `wamn_postgres.rs`: internal module split (1,510 lines)

The plugin is the security-critical file in the repo and is approaching the size where review quality degrades (M-BALANCED-MODULES). Split — same crate, no API change — into `plugins/wamn_postgres/{mod,types,pool,claims,resources}.rs`: WIT type conversion + error mapping (`types`), pool construction + destroy-on-abnormal-drop lifecycle (`pool`), claim injection (`claims` — which R2's `set_config` rewrite is about to touch anyway; do SR4 first or together so R2's diff reviews clean), transaction/cursor resource plumbing (`resources`). Pure mechanical moves; the S2 gates are the regression net. Ride-along: `wamn-api/src/router.rs` (933) is next in line for the same treatment when 4.2 lands auth — note it, don't do it yet.

---

## SR5 — `CronError(pub String)`: the one stringly-typed error

Every other error in the workspace is a structured enum whose variants an engine or gate folds mechanically (`NodeError`, `PgError`, `CompileError` ×3, `ReconstructError`, …). `CronError(pub String)` (run-queue `cron.rs`) breaks the pattern (M-STRONG-TYPES): callers can only log it, and dispatchbench can only string-match it. Replace with variants for its actual failure modes (invalid expression, tick computation overflow/ambiguity, anchor I/O passthrough) with `Display` preserved for logs. One-hour change, next dispatcher touch.

**Done** (wamn-qfr.4). `CronError` is now a three-variant enum — `InvalidExpression { schedule, detail }` (the `parse` site), `OutOfRangeInstant { ms }` (`to_dt`), `NoOccurrence { schedule, detail }` (both `find_*_occurrence` sites) — folded mechanically from each construction point, with `Display` preserving the `cron: …` log strings the dispatcher quarantine records. The doc's listed "anchor I/O passthrough" mode is deliberately **not** a variant: `cron.rs` is pure (house rule 1), so the dispatcher's anchor read is the driver's own `tokio_postgres` error in `crates/wamn-host/src/dispatch.rs`, not this type's concern. Pure-crate change (`cargo test/clippy/fmt -p wamn-run-queue`); consumers compile unchanged. This closes the SR series.

---

## SR6 — Write down the conventions that are currently implicit

The codebase has a strong, *consistent* house style that exists only by example. New contributors (human or agent) will drift it. Add a short "Code conventions" section to AGENTS.md/CLAUDE.md capturing, with one-line rationale each:

1. **Pure core / effect shell:** decision logic is clock-free, connection-free, unit-testable; effects live in drivers (run-queue, run-store, cron, f1, api all comply). New subsystems follow.
2. **Errors are enums mirroring WIT variants,** folded mechanically — a deliberate deviation from struct-error guidance (M-ERRORS-CANONICAL-STRUCTS), justified because the WIT boundary dictates variant shape and the runner's retry semantics consume it. Documenting the deviation keeps it a decision rather than a habit.
3. **SQL is pure text builders + `$n` params in crates;** whoever holds the connection executes. No inline SQL in components or drivers (SR2 completes the regime).
4. **Components are shells** (≤ a few hundred lines of dispatch glue); logic lives in crates (SR2's target as a standing rule).
5. **The bench/fixture/proof triple** is the gate pattern; gates live in `wamn-gates` (SR1); shared measurement code in `wamn-gate-harness`.
6. **Naming:** `wamn_` SQL-identifier prefix is platform-reserved (R9a); `WAMN_*` env vars, parsed via the harness; `poc-` component prefix (SR3).
7. **Drift guards over duplication bans:** where two representations must coexist (WIT ↔ SDK mirror, flow JSON ↔ fixtures), a coherence test guards them — name the existing ones as the pattern to copy.

---

## SR7 — WIT vendoring: optional consolidation

Each component vendors `wit/` copies of the frozen contract; `wit_coherence.rs` drift-guards all of them, which makes this low-priority. If the copy count grows with the node ecosystem: point `wit_bindgen::generate!(path: …)` at a single shared `wit/` directory (components workspace supports relative paths) and the coherence test shrinks to one comparison. Do it opportunistically; the guard means there is no correctness exposure today.

---

## Explicitly fine (looked at, no action)

- Two workspaces (native + wasm32) with separate lockfiles: necessary and correctly minimal; components profile (`opt-level = "s"`, strip) appropriate.
- Three `CompileError` types in three compiler crates: consistent naming, separate paths, no confusion in practice (M-SINGLE-ITEM-PATH satisfied per-crate).
- Workspace hygiene: shared edition (2024)/version/license, fork pin documented in-place with rationale, flat crate folder (M-CARGO-WORKSPACE et al.).
- The SDK boundary stack — `Node` trait, `purity.rs` dependency lint, `wit_coherence.rs`, `export_node!` one-macro componentization — is the strongest structure in the repo; SR6 exists to protect it, not change it.

## Sequencing
1. **SR3** (half day, pure moves) — do first; SR1/SR2's `git mv`s then land against the final layout.
2. **SR1** (1–2 days) — before the next bench is written.
3. **SR2** (1 day) — before F3/F4 flows; pairs naturally with SR1's harness extraction (flowrunner's bench helpers move in the same sweep).
4. **SR4** with/just before R2's claim rewrite; **SR5** with the next dispatcher touch; **SR6** anytime this week; **SR7** opportunistic.

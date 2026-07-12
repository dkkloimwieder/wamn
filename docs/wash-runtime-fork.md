# Consuming `wash-runtime` from the wamn fork

wamn builds against `wash-runtime` from **our fork of the wasmCloud monorepo**
— https://github.com/dkkloimwieder/wasmCloud — consumed as a plain cargo git
dependency. Upstream is `publish = false`, so a git dependency is the only way
to consume it; the fork is where our carried commits live.

- **Branch naming:** `wamn/X.Y.Z` = upstream release `runtime-operator/vX.Y.Z`
  + the carried wamn commits on top. Current: `wamn/2.5.2` = upstream v2.5.2
  (`ec012da`) + the epoch-deadline commit.
- **The pin:** `workspace.dependencies.wash-runtime.rev` in the root
  `Cargo.toml` — the **single source of truth**. Pin a **rev** (immutable),
  never a branch name (branches move); the branch's existence on the fork is
  what keeps the SHA fetchable. `crates/wamn-host/Cargo.toml` consumes it via
  `workspace = true`.
- **Features:** `default-features = false, features = ["washlet",
  "wasi-config", "wasi-logging", "wasi-otel"]`. Default features pull
  `wasi-webgpu`, which drags a *crates.io* wasmtime alongside the workspace's
  git-pinned one — two wasmtimes = linker typecheck failures.
- **wasmtime alignment:** wamn-host pins `wasmtime-wasi`/`wasmtime-wasi-http`
  to the exact git rev the fork's workspace uses (currently
  `7535c0255b2b84f6ae4de6034649ba2eeda84173`, wasmtime 46.0.0) so the graph
  carries **one** wasmtime. Re-verify on every rev bump:
  `grep -c '^name = "wasmtime"$' Cargo.lock` must be 1.

This replaces the earlier vendoring mechanism (`scripts/vendor-wasmcloud.sh` +
`patches/` + a `[patch]` redirect into a gitignored `vendor/` checkout),
deleted when the fork switch landed. History: `patches/README.md` at rev
`45d0668` and earlier.

## Carried commits (the ledger)

The fork carries **host-integration commits only** — things upstream should
arguably own. wamn features never land there. Each commit records its **exit
condition**: the upstream change that makes it deletable.

| Commit (on `wamn/2.5.2`) | What / why | Exit condition |
|---|---|---|
| `94bf77f` "wamn: plumb per-store epoch deadline in new_store_from_templates" | Functional. `new_store_from_templates` (the crate's single production store-creation site, `crates/wash-runtime/src/engine/linked_call.rs`) gives every store an epoch deadline: the active component's `wamn.epoch-deadline-ticks` config (from the WorkloadDeployment CRD's `localResources.config`), else `WAMN_EPOCH_DEADLINE_TICKS` env, else effectively unbounded (`u64::MAX / 2` — `u64::MAX` would wrap in `current_epoch + delta`). Without it stores keep wasmtime's default deadline of 0 and trap on the first tick, so epoch interruption (S2 chaos gate, hard cancellation) is unusable. One call site by design, to minimize rebase drift. | upstream ships native epoch-deadline support — delete the commit (the wamn-host ticker/config side stays as-is) |

Everything else epoch-related lives **unforked** in wamn-host:
`Config::epoch_interruption(true)` layers in via `EngineBuilder::with_config`,
and `spawn_epoch_ticker` drives the public `Engine::increment_epoch()`
(`crates/wamn-host/src/engine.rs`; `host --epoch-tick-ms`, 0 = off).

Retired with the vendoring mechanism: patch `0002-workspace-lints-warn-not-deny`
existed because a `[patch]` *path* dep got the monorepo's full `-D warnings`
lint set; as a git dep cargo builds the crate with `--cap-lints allow`, so the
lint relaxation is automatic. Only re-add (as a fork commit) if `-D warnings`
ever actually fires from the dependency build.

Planned next carried commit: the per-component memory `ResourceLimiter`
(wamn-bp4.1) — same file, same ledger rules.

## Sync runbook

**Triggers, in priority order:**

1. **wasmtime security advisory** touching our version line — immediate. Run
   `cargo audit` against the lockfile weekly until CI exists.
2. **Upstream minor release** — evaluate, don't chase; batch quarterly unless a
   needed fix or feature pulls the schedule in.
3. **WASI 0.3 / wasmtime major** milestone — planned work; coordinate with the
   `wamn:node` 0.2 contract revision.

**Steps per sync** (upstream releases X.Y.Z at rev `NEWREV`):

```bash
# in a fork clone (remote 'upstream' = wasmCloud/wasmCloud)
git fetch upstream
# 0. pre-read: what moved in the files we carry commits against?
git log --oneline <OLD_BASE>..NEWREV -- crates/wash-runtime/src/engine/
# 1. new branch from the new upstream point (fork main stays stale — irrelevant)
git checkout -b wamn/X.Y.Z NEWREV
# 2. carry the wamn commits forward
git cherry-pick <epoch-commit> [<limiter-commit> ...]
# 3. a conflict is a REVIEW EVENT, not a merge chore: upstream changed that
#    code for a reason — read it. Re-check each commit's EXIT CONDITION:
#    if upstream now does it, DROP the commit.
git push -u origin wamn/X.Y.Z
```

Then in wamn: bump `rev` to the new branch tip, re-align the wasmtime pin to
the new workspace's line, `cargo update -p wash-runtime`, rebuild, and run the
**upgrade gate subset** — deliberately not all of P0, just the fork-load-bearing
behaviors:

- **S1:** instantiation p50/p99 + cap-kill + the epoch-deadline demo
  (`wamn-host bench`) — phase 4 is the regression that the epoch commit is
  present *and functional*: without the deadline, stores trap on the first
  tick, so a lost commit fails loudly.
- **S2:** the chaos gate (epoch-kill mid-transaction ×100; destroy-never-repool)
  (`pgbench`).
- **S3:** kill/resume idempotency (`flowbench`).
- the **ResourceLimiter differentiation phase** once wamn-bp4.1 lands.

**Record per sync** (in the fork branch's final commit message or a
`docs/p0-results.md` addendum): date, base rev old→new, commits
carried/dropped, gate numbers old→new. Budget: an afternoon when clean.

**Rollback:** repoint wamn's `rev` at the previous `wamn/*` branch tip and
rebuild. Never delete a branch wamn ever pinned — they are cheap and they are
the bisect trail.

**Drift check between syncs** (before host-touching feature work):
`git fetch upstream && git log --oneline <BASE>..upstream/main --
crates/wash-runtime/src/engine/` — a heads-up that carried-against code moved,
before an advisory puts a clock on the upgrade.

**Escalation threshold (standing):** if the fork grows past ~4–5 carried
commits, or the same commit conflicts on consecutive syncs, that is no longer
dependency management — engage upstream or explicitly accept
runtime-maintainer status as a decision-table entry.

## Sync log

| Date | Base | Carried | Gates |
|---|---|---|---|
| 2026-07-12 | `8b53285` (pre-2.5.2, vendored+patched) → v2.5.2 `ec012da` (fork `wamn/2.5.2` @ `94bf77f`) | epoch commit (was patch 0001; byte-identical diff); patch 0002 retired (git-dep `--cap-lints`); upstream wash-runtime delta = 1 commit (P3 `wasi:http/handler` routing fix `5ad4841`) | all PASS (debug build): S1 instantiation p99 367µs + cap-kill + epoch-kill (carried commit functional); S2 chaos ×100; S3 resume 10/10 (wamn-bp4.2) |

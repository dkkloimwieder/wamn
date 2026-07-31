# Consuming `wash-runtime` from the wamn fork

> **§1.9a audit (2026-07-19): amendments are additive — base sound.**

wamn builds against `wash-runtime` from **our fork of the wasmCloud monorepo**
— https://github.com/dkkloimwieder/wasmCloud — consumed as a plain cargo git
dependency. Upstream is `publish = false`, so a git dependency is the only way
to consume it; the fork is where our carried commits live.

- **Branch naming:** `wamn/X.Y.Z` = the peeled upstream `vX.Y.Z` tag + the
  carried wamn commits on top. Current: `wamn/2.6.1` = upstream v2.6.1
  (`df8a8bcd69adc9c23ded842e504071a5272d04ed`) + seven carried policy
  commits, at final fork tip `09b1132f2bab36e6e71f4637bd0e4755e359dd43`.
  The tip also contains proof/restoration fixes `f9fcf287` and `09b1132f`;
  neither is an additional carried-policy row.
- **The pin:** `workspace.dependencies.wash-runtime.rev` in the root
  `Cargo.toml` — the **single source of truth**. Pin a **rev** (immutable),
  never a branch name (branches move); the branch's existence on the fork is
  what keeps the SHA fetchable. `services/host/Cargo.toml` consumes it via
  `workspace = true`.
- **Features:** `default-features = false, features = ["washlet",
  "wasi-config", "wasi-logging", "wasi-otel"]`. Default features pull
  `wasi-webgpu`, which remains excluded from the base upgrade; disabling
  defaults is feature posture, not a source-identity workaround.
- **Wasmtime alignment:** wamn and `wash-runtime` resolve one crates.io
  Wasmtime 47.0.1 family. Re-verify that production and proof packages retain
  that single type universe on every rev bump with the executable gate:
  `cargo test -p wamn-proof-conformance --test wasmtime_source_identity`.

This replaces the earlier vendoring mechanism (`scripts/vendor-wasmcloud.sh` +
`patches/` + a `[patch]` redirect into a gitignored `vendor/` checkout),
deleted when the fork switch landed. History: `patches/README.md` at rev
`45d0668` and earlier.

## Current upstream delta (v2.6.0 → v2.6.1)

The target was verified from Git refs, not the incomplete GitHub releases page:

```text
$ git ls-remote --tags https://github.com/wasmCloud/wasmCloud.git \
    'refs/tags/v2.6.1' 'refs/tags/v2.6.1^{}'
001067539ac8638b8987d10e34c2c247a1798138  refs/tags/v2.6.1
df8a8bcd69adc9c23ded842e504071a5272d04ed  refs/tags/v2.6.1^{}
```

The upstream delta is exactly three linear commits:

| Commit | Change |
|---|---|
| `dd8eccedd9eb96458fa6443fb940ba4bfb57667a` | Rename `HttpServer` to `Ingress` and `HttpServerBuilder` to `IngressBuilder`. |
| `9c7aa1fa9dbe6ab243797a655e74b6049aaa8193` | Rename `AllowIPNameLookup` to `AllowedIPNameLookups`, JSON/YAML `allowIpNameLookup` to `allowedIpNameLookups`, and Rust/internal state to `allowed_ip_name_lookups`. |
| `df8a8bcd69adc9c23ded842e504071a5272d04ed` | Bump the release version to v2.6.1. |

These are two naming changes and a release bump. The dependency target remains
the Wasmtime family at 47.0.1, `async-nats` 0.49.1, and `rust-version` 1.94.0.
All seven carried-policy exit conditions remain unchanged and unsatisfied.

## Carried commits (the ledger)

The fork carries **host-integration commits only** — things upstream should
arguably own. wamn features never land there. Each commit records its **exit
condition**: the upstream change that makes it deletable.

| Commit (on `wamn/2.6.1`) | What / why | Exit condition |
|---|---|---|
| `f90d977f` "fix(wash-runtime): restore epoch deadline policy (wamn-g2br.2)" | Functional. On v2.6.1, `new_store_from_templates` remains the single production policy call site for enabled workload-store paths. It gives every newly created store a finite epoch deadline: the active component's `wamn.epoch-deadline-ticks` config, else `WAMN_EPOCH_DEADLINE_TICKS`, else effectively unbounded (`u64::MAX / 2`; `u64::MAX` would wrap in `current_epoch + delta`). Without it, Wasmtime's default deadline is 0 and epoch-enabled stores trap on the first tick. Warm pooling, trigger/service adoption, and host-component plugin stores remain disabled in the base upgrade; reusable-store adoption must re-arm the deadline per checkout. `NodeRuntime` is unchanged. One production call site is preserved by design to minimize rebase drift. | upstream ships native epoch-deadline support — delete the commit (the wamn-host ticker/config side stays as-is) |
| `24b220f5` "fix(wash-runtime): restore memory limiter policy (wamn-g2br.3)" | Functional. On v2.6.1, `new_store_from_templates` remains the single production policy call site for enabled workload-store paths. It resolves the active component's per-linear-memory budget from first-class `memory_limit_mb`, else `wamn.memory-limit-mb` config, else `WAMN_MEMORY_LIMIT_MB`; only a configured budget attaches `WamnStoreLimiter` through `Store::limiter`, so unbudgeted stores retain upstream behavior. Growth above budget is denied, logged on `wamn::memory`, and counted; a fixed 500,000-element table cap rides with budgeted stores. A budget above host-advertised `WAMN_MEMORY_CEILING_MB` fails store construction descriptively and is never clamped. Warm pooling, trigger/service adoption, and host-component plugins remain disabled in the base upgrade; aggregate `pool_size × budget` enforcement is a reusable-store adoption prerequisite. `NodeRuntime` is unchanged. | upstream plumbs `memory_limit_mb` into a Store limiter — delete the commit |
| `6ca3d6f7` "feat(wash-runtime): restore outbound trace injection (wamn-g2br.4)" | Functional. On v2.6.1, the `Ingress` P2 and P3 host surfaces inject the current W3C trace context at their common pre-transport seams, before the HTTP/gRPC/custom-transport branch. P2 initially carries caller context so custom transports preserve continuity; built-in P2 HTTP and gRPC transports re-inject client-span context for correct downstream parenting. P3 creates its client span before the common seam, so P3 HTTP, gRPC, and custom transports receive client-span context. This is host-enforced for every admitted outbound request dispatched through those surfaces; `DefaultOutgoingHandler` is not universal on v2.6.1. The global no-op propagator injects nothing when observability is off. | upstream provides equivalent host-enforced P2/P3 trace-context injection across HTTP, gRPC, and custom transports, including client-span parenting — delete the commit |
| `0d98f850` "fix(runtime): restore raw TCP denial (wamn-g2br.5)" | Security. On v2.6.1, `wasi:sockets` remains linked independently of the `wasi:http` egress allowlist, so `build_ctx_from_template` denies `TcpConnect` unless the active component opts in. `wamn.allow-raw-sockets` config takes precedence over `WAMN_ALLOW_RAW_SOCKETS`; absent or unparseable values deny. Literal and loopback TCP are denied without opt-in, and the first denial warns once per component under `wamn::sockets`. `AllowedIPNameLookups` is an independent primitive and does not satisfy this policy. The upstream P3 loopback-gateway fixture explicitly declares the raw-socket capability it consumes. UDP behavior remains upstream in this commit and is owned separately by wamn-g2br.6. | upstream gates socket linking on `host_interfaces`, or consults an egress policy for `TcpConnect` — delete the commit |
| `a9f9c57d` "fix(runtime): deny raw UDP without opt-in (wamn-g2br.6)" | Security. On v2.6.1, `UdpConnect` and `UdpOutgoingDatagram` share the raw-socket opt-in resolved as component config, then `WAMN_ALLOW_RAW_SOCKETS`, then DENY; unparseable values fail closed. `UdpBind` is service-loopback-only and denied for non-service components, and raw-egress opt-in never widens bind authority. P2 unconnected sends explicitly consult `UdpOutgoingDatagram`. P3 unconnected sends consult `UdpOutgoingDatagram` before socket-state inspection, then separately consult `UdpBind` before the one-time implicit bind, preserving both ordered denial points. Warn-once visibility now covers TCP and UDP raw egress. `AllowedIPNameLookups` remains independent. | upstream gates socket linking on `host_interfaces`, or consults an egress policy for UDP connect/datagram operations — delete the commit |
| `95b04ded` "feat(wash-runtime): expose limiter accessors (wamn-g2br.7)" | Observability (9.8, wamn-jn6), accessor half split from the former combined `981fdc5` seam. `WamnStoreLimiter` exposes read-only `component_id`, `budget_bytes`, `high_water_bytes`, and `denied_total` getters so a host observable instrument can bridge one store lifetime's per-linear-memory state to `wamn.memory.*` metrics without re-parsing `wamn::memory` denial logs. Fields remain private and limiter behavior is unchanged. This row is independent of the inbound HTTP request-counter seam and cannot be retired by upstream HTTP metrics alone. | upstream exposes equivalent limiter introspection accessors — delete the commit |
| `33b24183` "feat(wash-runtime): count inbound API requests (wamn-g2br.8)" | Observability (9.8, wamn-jn6), request-counter half split from the former combined `981fdc5` seam. The `Ingress` response-status choke point increments the global-meter `wamn.api.requests` `u64` counter with only the bounded `status_class` label. It counts routing failures, long-lived service HTTP responses, ordinary P2/P3 responses, and warm P3 responses exactly once. It is a no-op until observability installs a provider. This row is independent of the limiter-accessor seam and cannot be retired by accessor evidence. | upstream provides an equivalent inbound request-count metric, and the wamn dashboards, SLOs, and mutation gate have migrated to it and passed — delete the commit |

Everything else epoch-related lives **unforked** in wamn-host:
`Config::epoch_interruption(true)` layers in via `EngineBuilder::with_config`,
and `spawn_epoch_ticker` drives the public `Engine::increment_epoch()`
(`crates/platform/runtime/src/engine.rs`; `host --epoch-tick-ms`, 0 = off).

Retired with the vendoring mechanism: patch `0002-workspace-lints-warn-not-deny`
existed because a `[patch]` *path* dep got the monorepo's full `-D warnings`
lint set; as a git dep cargo builds the crate with `--cap-lints allow`, so the
lint relaxation is automatic. Only re-add (as a fork commit) if `-D warnings`
ever actually fires from the dependency build.


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
# verify the annotated tag and its peeled commit directly; the releases page is
# not authoritative and currently omits the v2.5.x/v2.6.x tags
git ls-remote --tags https://github.com/wasmCloud/wasmCloud.git \
  'refs/tags/vX.Y.Z' 'refs/tags/vX.Y.Z^{}'
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

Then in wamn: bump `rev` to the new branch tip, re-align the workspace's
crates.io `wasmtime-wasi` and `wasmtime-wasi-http` versions to the exact
release `wash-runtime` resolves, and run
`cargo update -p wash-runtime`. Before rebuilding, run
`cargo test -p wamn-proof-conformance --test wasmtime_source_identity`; then
run the **upgrade gate subset** — deliberately not all of P0, just the
fork-load-bearing behaviors:

- **S1:** instantiation p50/p99 + cap-kill + the epoch-deadline demo
  (`wamn-gates bench`) — phase 4 is the regression that the epoch commit is
  present *and functional*: without the deadline, stores trap on the first
  tick, so a lost commit fails loudly.
- **S2:** the chaos gate (epoch-kill mid-transaction ×100; destroy-never-repool)
  (`pgbench`).
- **S3:** kill/resume idempotency (`flowbench`).
- **bench phase 5:** the ResourceLimiter differentiation gate (concurrent
  64/192 MiB budgets each trap at their own number; unbudgeted at the
  ceiling; over-ceiling never allocates) — the regression that the limiter
  commit is present *and functional*: on upstream, the budgeted memhogs
  would run to the ceiling and the phase fails loudly.

**Record per sync** in the sync log below: date, verified tag object and peeled
base rev old→new, commits carried/dropped, and gate numbers old→new. Budget: an
afternoon when clean.

**Rollback:** repoint wamn's `rev` at the previous `wamn/*` branch tip and
rebuild. Never delete a branch wamn ever pinned — they are cheap and they are
the bisect trail.

**Drift check between syncs** (before host-touching feature work):
`git fetch upstream && git log --oneline <BASE>..upstream/main --
crates/wash-runtime/src/engine/` — a heads-up that carried-against code moved,
before an advisory puts a clock on the upgrade.

**Escalation threshold — RESOLVED as D23 (owner, 2026-07-19):**
**runtime-maintainer status accepted.** The fork is a first-class owned
component, not managed dependency drift: the ~4–5 carried-commit ceiling is
retired, and the **upgrade-gate subset above is the standing sync gate** —
every base-rev sync or carried-commit addition runs it and appends to the
sync log, exactly as practiced. Upstreaming individual commits stays welcome
opportunistically but is no longer a forcing function. (Recorded in the
`docs/platform-plan.md` decision table as D23; the historical threshold text
this replaces: past ~4–5 carried commits or repeated sync conflicts, engage
upstream or accept maintainer status.)

## Sync log

| Date | Base | Carried | Gates |
|---|---|---|---|
| 2026-07-12 | `8b53285` (pre-2.5.2, vendored+patched) → v2.5.2 `ec012da` (fork `wamn/2.5.2` @ `94bf77f`) | epoch commit (was patch 0001; byte-identical diff); patch 0002 retired (git-dep `--cap-lints`); upstream wash-runtime delta = 1 commit (P3 `wasi:http/handler` routing fix `5ad4841`) | all PASS (debug build): S1 instantiation p99 367µs + cap-kill + epoch-kill (carried commit functional); S2 chaos ×100; S3 resume 10/10 (wamn-bp4.2) |
| 2026-07-12 | base unchanged (v2.5.2); fork `wamn/2.5.2` advanced `94bf77f` → `5b158ff` | + memory-limiter commit (wamn-bp4.1/D16) | all PASS (debug build): bench phases 1–5 incl. the new differentiation gate (budget-64 → 56 MiB, budget-192 → 184 MiB, unbudgeted → 248 MiB at the ceiling, over-ceiling never allocated); S2 chaos + S3 resume regression |
| 2026-07-15 | base unchanged (v2.5.2); fork `wamn/2.5.2` advanced `5b158ff` → `d3d83f3` | + outbound-traceparent commit (wamn-rvd/9.2); now **3** carried commits (under the ~4–5 escalation threshold) | wamn-host debug build PASS against the new rev; 9.2 `traceproof` in-cluster gate of record PASS (deployed cross-pod host-enforced inject); regression by non-change (host code unchanged — only the consumed wash-runtime rev moved) |
| 2026-07-19 | base unchanged (v2.5.2); fork `wamn/2.5.2` advanced `d3d83f3` → `8b76869` | + TcpConnect deny-unless-opt-in commit (E13/wamn-7j0.1); now **4** carried commits — **AT the ~4–5 escalation threshold** (flagged: consider engaging upstream on socket-policy gating) | wamn-host debug build + 68 lib tests PASS against the new rev; single-wasmtime lock check PASS; the negative runtime gate (raw-socket component denied / opted-in component connects) rides the next image rebake (wamn-2jkm.41) — no in-crate test harness exists at `linked_call.rs` |
| 2026-07-19 | base unchanged (v2.5.2); fork `wamn/2.5.2` advanced `8b76869` → `eef76cd` | + UDP socket-policy commit (E15/E16, wamn-7j0.2); now **5** carried commits — **PAST the ~4–5 escalation threshold: engage upstream on socket-policy gating (or accept runtime-maintainer status as a decision-table entry) before the next carried commit** | in-fork: 21/21 `linked_call::tests` PASS (debug — the first in-crate suite at this layer); wamn side: lock refresh via `cargo check`, single-wasmtime lock check, wamn-host debug lib tests against the new rev (see the pin-bump commit); the UDP negative runtime gate (egressbench: socket-importing component cannot send UDP to non-loopback) rides the next image rebake (wamn-2jkm.41) |
| 2026-07-22 | base unchanged (v2.5.2); `eef76cd` → `981fdc5` on lane branch `wamn/jn6-metrics` | + memory-limiter accessors + `wamn.api.requests` host HTTP counter (9.8, wamn-jn6); now **6** carried commits (D23 runtime-maintainer status accepted — the ceiling is retired) | wamn side (debug): pin bumped to `981fdc5`, single-wasmtime lock check PASS, `cargo test -p wamn-host -p wamn-executor -p wamn-dispatcher -p wamn-gates` PASS against the new rev; 9.8 `metricbench` local phases 1-5 PASS + 3 mutants killed (outcome-fold / depth-predicate / memory budget-vs-high-water). **INTEGRATOR: push `wamn/jn6-metrics` (or rebase `981fdc5` onto `wamn/2.5.2`) BEFORE cherry-picking the wamn pin-bump commit** — the SHA is not yet on the remote. The `wamn.api.requests` counter's in-cluster negative/positive gate rides the next image rebake (metricbench phase 6). |
| 2026-07-30 | v2.5.2 `ec012da` (fork `wamn/2.5.2` @ `981fdc5`) → v2.6.0 `9bf8e97` (fork `wamn/2.6.0` @ `0928c3e`) | all six policies re-ported as seven commits: epoch `7bf5e9ab`, memory limiter `a2a1ef16`, outbound trace `0aee2546`, raw TCP `89d24ebc`, raw UDP `11bef6ee`, and former combined `981fdc5` split into limiter accessors `d1f862c5` plus request counter `0928c3ec`; dropped: none | all PASS at wamn `f68de90` on exact gates image `sha256:5a2f8599d357367c8d0693bd115ba92f860afadda0778945df7235cdc23ba437` (runtime ImageID `sha256:82b90591cf9c7ed6b554f0a4ece494a7f391a1d32218da1b7bfaf36debedf5b5`): locked debug proofs type identity 3/3, conformance 60/60, integration 82/82, system 64/64; S1 p99 367µs debug → 12.923µs exact-image, limiter 56/184/248 → 56/184/248 MiB, over-ceiling none → none, cap and epoch `Trap::Interrupt` PASS; S2 12,938 qps, p99 3.555ms, chaos 100/100, zero RLS leaks and injection mismatches; S3 p99 3.151µs, hot reload 2.755ms, resume 10/10 → 10/10; nodebench, egressbench, P2/P3 socketguard, and metricbench phases 1–5 PASS (phase 6 documented skip, queue drained, zero DB residue); exact-image callable F0–F4 PASS with receipt `sha256:a4b98231eef6c651f1335ae4590ceeb7ddf4982c1b6a8c5cfc427e12ac3`; protected UIDs unchanged |
| 2026-07-31 | v2.6.0 `9bf8e97` (fork `wamn/2.6.0` @ `0928c3e`) → v2.6.1 `df8a8bcd` (fork `wamn/2.6.1` @ `09b1132f`); `git ls-remote --tags` verified tag object `00106753` and peeled commit `df8a8bcd` | upstream delta: `dd8ecced` renamed `HttpServer` → `Ingress`; `9c7aa1fa` renamed `AllowIPNameLookup` / `allowIpNameLookup` → `AllowedIPNameLookups` / `allowedIpNameLookups` / `allowed_ip_name_lookups`; `df8a8bcd` bumped the release. All seven policy seams were re-ported as `f90d977f`, `24b220f5`, `6ca3d6f7`, `0d98f850`, `a9f9c57d`, `95b04ded`, and `33b24183`; dropped: none. Proof/restoration fixes `f9fcf287` and `09b1132f` are not policy rows. Dependencies are unchanged: Wasmtime 47.0.1, `async-nats` 0.49.1, and `rust-version` 1.94.0; all seven exit conditions are unchanged. | fork seam tests, full `wash-runtime` debug suites/builds, rustdoc, and controlled mutations PASS through `09b1132f`; wamn locked-debug pin/migration matrix PASS at `3c933cf`; focused ledger/governance conformance and mutations PASS under wamn-g2br.10. Exact-image closure receipts are deliberately owned by wamn-g2br.11 and are not recorded early. |

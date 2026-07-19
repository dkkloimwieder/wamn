# Consuming `wash-runtime` from the wamn fork

> **§1.9a audit (2026-07-19): amendments are additive — base sound.**

wamn builds against `wash-runtime` from **our fork of the wasmCloud monorepo**
— https://github.com/dkkloimwieder/wasmCloud — consumed as a plain cargo git
dependency. Upstream is `publish = false`, so a git dependency is the only way
to consume it; the fork is where our carried commits live.

- **Branch naming:** `wamn/X.Y.Z` = upstream release `runtime-operator/vX.Y.Z`
  + the carried wamn commits on top. Current: `wamn/2.5.2` = upstream v2.5.2
  (`ec012da`) + the epoch-deadline, memory-limiter, and outbound-traceparent
  commits.
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
| `5b158ff` "wamn: enforce per-component memory budgets via a store ResourceLimiter" | Functional (D16, wamn-bp4.1). Same store-creation site + `engine/ctx.rs`: resolves a per-component budget — the spec's first-class `memory_limit_mb` (upstream's dead field), else `wamn.memory-limit-mb` config, else `WAMN_MEMORY_LIMIT_MB` env — and, only when one is configured, attaches a `WamnStoreLimiter` via `Store::limiter`. Grow past the budget is denied (in-guest failure → allocator abort → service trap, the same failure shape as the engine cap); a budget above the host-advertised `WAMN_MEMORY_CEILING_MB` (set by wamn-host from its pooling cap; the allocator cap is not introspectable and `memory_growing`'s `maximum` is only the declared max) fails store creation with a descriptive error — never a silent clamp; denials are logged + counted (target `wamn::memory`); a fixed table-elements cap rides along. v0 is per-linear-memory, not store-cumulative. Unbudgeted stores attach no limiter — byte-identical to upstream. The most upstreamable commit we carry (implements their own carried-but-dead field). | upstream plumbs `memory_limit_mb` into a Store limiter — delete this commit |
| `d3d83f3` "wamn: inject W3C trace context on outbound HTTP in DefaultOutgoingHandler" | Functional (9.2, wamn-rvd). `host/http.rs` `DefaultOutgoingHandler::send_request` — the single production outbound `wasi:http` send path (reached via `CtxHttpHooks` → `HttpServer::outgoing_request` → the default handler) — injects the current W3C trace context (`get_text_map_propagator().inject_context` over the outbound client span's `span.context()`) onto the request headers before send. The inbound path already *extracts* `traceparent` (`handle_http_request`, same file); this is the symmetric outbound *inject*, so a trace continues across a process boundary. Host-enforced: every outbound call flows through this handler regardless of whether the guest used an SDK, so an SDK-bypassing custom node cannot break trace continuity. A no-op when observability is off (the global propagator is a `Noop`, injecting nothing) — byte-identical to upstream then. All three deps (`tracing-opentelemetry`, `opentelemetry`, `opentelemetry-http`) were already in the crate. | upstream injects outbound W3C trace context in its default outgoing handler — delete this commit |
| `8b76869` "wamn: deny wasi:sockets TcpConnect unless the workload opts in" | Security (E13, wamn-7j0.1). `wasi:sockets` is linked into every component unconditionally (`engine/mod.rs` links tcp/udp/create-socket/instance-network/network/ip_name_lookup + `add_p3_to_linker`, no `host_interfaces` gate) and the parsed egress allowlist (`allowed_hosts`) governs the `wasi:http` path only, so a guest could open a raw TCP socket to any post-DNS address and bypass egress policy entirely. `build_ctx_from_template`'s `socket_addr_check` (`crates/wash-runtime/src/engine/linked_call.rs`, the single production ctx-build site) now denies `SocketAddrUse::TcpConnect` unless the workload opts in — the active component's `wamn.allow-raw-sockets` config wins, then `WAMN_ALLOW_RAW_SOCKETS` env, else DENY (unparseable = deny, a security floor), following the epoch/limiter config-read precedent in the same file. Not allowlist matching: `allowed_hosts` is name-shaped but `TcpConnect` sees a post-DNS `SocketAddr`, so proper matching would need an `ip_name_lookup` hook and name→IP allowlists are fragile — a binary opt-in is the honest policy at this layer. Denials are visible: `warn!` once per component (target `wamn::sockets`, matching the limiter's `wamn::memory`). Other arms untouched (upstream): `TcpBind` service-loopback-only, `UdpBind` loopback/unspecified, outbound UDP (`UdpConnect`/`UdpOutgoingDatagram`) still allowed — a separate hole, tracked as its own bead. | upstream gates socket linking on `host_interfaces`, or consults an egress policy for `TcpConnect` — delete this commit |
| `eef76cd` "wamn: deny raw wasi:sockets UDP egress + tighten UdpBind (E15/E16 follow-up)" | Security (E15 High / E16 Med, wamn-7j0.2). The follow-up `8b76869`'s message named: same `socket_addr_check` closure, `crates/wash-runtime/src/engine/linked_call.rs`. `UdpConnect`/`UdpOutgoingDatagram` (previously `true` unconditionally — raw UDP egress to any post-DNS address, the TCP hole's twin; enforced at `host_udp.rs` + the p3 mirror `host_udp_p3.rs`) now share the SAME `wamn.allow-raw-sockets` opt-in (config > `WAMN_ALLOW_RAW_SOCKETS` env > DENY; unparseable denies). `UdpBind` (previously loopback-or-unspecified for EVERY component — an all-interfaces inbound UDP listener) tightened to match `TcpBind`: service loopback-only, non-service denied. The decision is factored into pure fns (`resolve_allow_raw_sockets`, `socket_addr_permitted`) with a 21-test in-crate suite — the first carried commit with unit coverage at this layer; TCP return values byte-equivalent, warn-once now covers all raw egress (`reason`-tagged). Connected-socket datagrams ride the checked `UdpConnect` address; the unconnected per-datagram path is the checked `UdpOutgoingDatagram` case — no gap. | upstream gates socket linking on `host_interfaces`, or consults an egress policy for the connect/datagram ops — delete this commit |

Everything else epoch-related lives **unforked** in wamn-host:
`Config::epoch_interruption(true)` layers in via `EngineBuilder::with_config`,
and `spawn_epoch_ticker` drives the public `Engine::increment_epoch()`
(`crates/wamn-host/src/engine.rs`; `host --epoch-tick-ms`, 0 = off).

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

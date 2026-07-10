# P0 Spike Results — Raw Numbers

Records measurements against `docs/p0-exit-criteria.md`. One section per spike;
the cross-cutting exit (decision closure for D5/D7/design-note-9b) happens in
[P0-EXIT] once all spikes report.

## Bench environment

- Dev workstation: 11th Gen Intel i7-1185G7 (8 threads), 60 GiB RAM, Ubuntu 26.04, Linux 7.0.0-22
- Cluster: kind v0.32.0, Kubernetes v1.36.1, 3 nodes (1 control-plane + 2 workers), docker 28.5.1
- Toolchain: Rust 1.97.0; wasmtime 46.0.0 (git `7535c025`, via wash-runtime's workspace pin)
- Runtime: wash-runtime 2.5.1, git `wasmCloud/wasmCloud@8b53285f` (crate is `publish = false` upstream — git dependency is the only option)
- Operator: runtime-operator Helm chart 2.5.2 from `oci://ghcr.io/wasmcloud/charts/runtime-operator` (verified template-identical to the chart at the pinned runtime rev)

## S1 — Custom host image (1.3) — **PASS** (2026-07-09)

**Deliverable shipped:** `wamn-host` binary (crates/wamn-host) embedding
`wash_runtime::washlet::ClusterHostBuilder` with plugins registered:
`wasi:http` (DynamicRouter server), `wasi:config` (DynamicConfig),
`wasi:logging` (TracingLogger), `wasi:otel`, `wamn:postgres` (stub, canned
results — real implementation is S2), `wamn:node/control` (stub). Host-process
OTel via `initialize_observability` (activates on `OTEL_*` env). OCI image
built from `Dockerfile` (152 MB), deployed via the runtime-operator chart with
custom image values (`deploy/values-wamn.yaml`); 3 host pods self-registered
as `Host` CRs (READY=True) and served a `WorkloadDeployment` end-to-end
(`curl -H "Host: hello.localhost.direct" http://127.0.0.1:80/` → HTTP 200
through NodePort 30950).

**Engine config:** pooling allocator, `max_memory_size = 256 MiB`, 512 slots
(memories/tables/component-instances/stacks). Benches run with
`wamn-host bench` (in-image fixtures), methodology mirrors upstream's
`wasmtime_baseline` bench: cold instantiation = `Store::new` +
`CommandPre::instantiate_async` on a pre-compiled component, one instance per
store, matching the runtime's per-invocation serving strategy.

| Measure | In-cluster (kind pod) | Local (workstation) | Gate | Verdict |
|---|---|---|---|---|
| Cold instantiation p50 | 6.1 µs | 5.6 µs | — | — |
| Cold instantiation p99 | 25.3 µs | 10.2 µs | < 10 ms | **PASS** (~400× headroom) |
| Cold instantiation max | 35.7 µs | 21.7 µs | — | — |
| Memory @ 100 resident components | 46.7 MiB total, 0.47 MiB/component | same | host stable | **PASS** |
| Workload start (compile + resolve) | 80.5 ms/workload | 60.2 ms/workload | — | — |
| 256 MiB cap kill | clean guest trap at 248→256 MiB; heartbeat OK; host accepts new work | same | no host restart | **PASS** |

Notes on method: density used a **unique digest per workload** to defeat the
runtime's compile cache — 100 separately-compiled resident components, the
honest multi-tenant case. The test component is minimal (~65 KB wasi:cli
command); real components will carry more compiled-code residency. Cap kill:
the guest's `memory.grow` fails at the pooling cap, Rust's allocator aborts,
the service traps; with `max_restarts: 0` the host logs and moves on.

**Upstream gaps found (feed into S2/S3 planning and [P0-EXIT]):**

1. **No epoch interruption anywhere in wash-runtime 2.5.1** — nothing calls
   `set_epoch_deadline`/`increment_epoch`; `Config::epoch_interruption` is
   never set, and stores are created inside the crate (no injection point
   without a fork/PR). The S2 chaos test ("epoch-kill a component
   mid-transaction") and the platform's hard-cancellation layer
   (wamn-node design note 3) **depend on this**. **RESOLVED** by carried
   patch (wamn-4p3) — see the follow-up section below.
2. **No per-component memory limits** — `LocalResources.memory_limit_mb` is
   carried but never plumbed into wasmtime; no `ResourceLimiter`/
   `Store::limiter` call sites. The 256 MiB cap here is the pooling
   allocator's engine-wide `max_memory_size` — uniform across all components
   on a host, not per-workload-differentiable.
3. **Stores and `InstancePre` are built per invocation**; `Component.pool_size`
   / `max_invocations` are dead TODOs upstream. Fine at current numbers
   (instantiation is µs-scale), relevant to S3 dispatch-overhead budgeting.
4. Workload status remains `Running` after a service dies with
   `max_restarts` exhausted (state-accounting nit, cosmetic for S1).

**Fail branch:** not taken.

### Follow-up: epoch interruption via carried patch (wamn-4p3) — **DONE** (2026-07-10)

**Decision (user): carried patch only, no upstream PR.** Only one of the three
required pieces touches upstream code:

1. *No patch* — `Config::epoch_interruption(true)` layers onto the engine via
   `EngineBuilder::with_config` (base config; pooling/proposals stack on top) —
   `crates/wamn-host/src/engine.rs`.
2. *No patch* — a tokio task drives the public `Engine::increment_epoch()`
   every 10 ms (`spawn_epoch_ticker`; `host` flag `--epoch-tick-ms`, 0 = off).
3. *Patch* — `patches/0001-wash-runtime-store-epoch-deadline.patch` adds one
   call in `new_store_from_templates` (`crates/wash-runtime/src/engine/
   linked_call.rs`, the crate's single production store-creation site): each
   store gets `set_epoch_deadline(ticks)` from the active component's
   `wamn.epoch-deadline-ticks` config — plumbed end-to-end from the
   WorkloadDeployment CRD's `localResources.config` — else the
   `WAMN_EPOCH_DEADLINE_TICKS` env var, else effectively unbounded
   (`u64::MAX / 2`; `u64::MAX` would wrap in wasmtime's
   `current_epoch + delta`). Without the patch, stores keep wasmtime's
   default deadline of 0 and trap on the first tick.

Deadline semantics: stores are per-invocation (gap #3), so N ticks × tick
period ≈ a wall-clock cap per invocation (per service run for services).

**Build mechanics:** `scripts/vendor-wasmcloud.sh` clones the pinned monorepo
rev into `vendor/wasmcloud` (gitignored) and applies `patches/*.patch`; the
root `Cargo.toml` `[patch]` section redirects the git dep to that checkout
(inside the real monorepo so `workspace = true` deps resolve — `vendor` is
excluded from our workspace for the same reason). The Dockerfile runs the same
script, so image builds are reproducible.
`patches/0002-workspace-lints-warn-not-deny.patch` relaxes the monorepo's
`-D warnings`: path deps don't get the `--cap-lints allow` that git deps get,
and our feature subset legitimately leaves some upstream code unused.

**Demo (bench phase 4):**

| Measure | Local | In-cluster (kind pod) |
|---|---|---|
| busyloop raw store, deadline = 20 ticks × 10 ms | killed at 195 ms as `Trap::Interrupt` | killed at 190.9 ms as `Trap::Interrupt` |
| busyloop service, `wamn.epoch-deadline-ticks: 100` | dies at ~1 s; heartbeat OK; host accepts new work | same |
| hello + workloads under running ticker (default deadline) | unaffected; S1 numbers unchanged (p50 5.8 µs / p99 10.3 µs) | unaffected (p50 5.5 µs / p99 9.2 µs); 3 chart-deployed hosts READY with ticker on; hello serves HTTP 200 |

The S2 chaos gate ("epoch-kill a component mid-transaction, 100×") is now
unblocked. Hard cancellation for wamn-node (design note 3): a short per-store
deadline caps any invocation's runtime; kill-on-demand can later be layered on
by tracking live stores — that needs no further upstream changes.

## S2 — wamn:postgres plugin (2.1–2.2) — pending

## S3 — Flow-runner PoC (5.2) — pending

## S4 — Custom-node invocation + config parse (5.6, D7, note 9b) — pending

## S5 — Logging capture (9.3) — pending

## S6 — Test host plugin-swap (11.1) — pending

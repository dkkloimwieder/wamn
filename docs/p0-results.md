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

## S2 — wamn:postgres plugin (2.1–2.2) — **PASS** (2026-07-10)

**Deliverable shipped:** the real `wamn:postgres` host plugin
(`crates/wamn-host/src/plugins/wamn_postgres.rs`) implementing the full
`wamn-postgres.wit` surface — `query`/`execute` (single statement in an
implicit, claim-injected, auto-committed transaction), explicit `transaction`
(query/execute/open-cursor/commit/rollback), and server-side `cursor` (bounded
`fetch`). Backed by a `deadpool-postgres` pool over `tokio-postgres`. Driver
choice (user): tokio-postgres + deadpool over sqlx — the plugin needs
`Object::take` (destroy-never-repool) for the chaos gate and raw `SQLSTATE` +
constraint names for the `pg-error` taxonomy, and D8's future user-authored SQL
runs through the same parameterized path regardless. Host-enforced invariants,
all verified below: guest never holds a socket (resource handles only); RLS
claims come from workload identity (`Ctx::component_id` → tenant → `SET LOCAL
app.tenant`) with no guest override; `statement_timeout` + row limit applied
host-side; abnormal instance death destroys the connection; parameters are
bound values only, never interpolated.

**Method:** a new `wamn-host pgbench` subcommand instantiates the `pgprobe`
guest (`components/pgprobe`, which imports `wamn:postgres/client`) into a
hand-built `SharedCtx` store with the plugin linked, and drives its
`run(op,arg)` export — "sustained qps from one component" per the spike. The
same harness hosts the three security gates. The in-cluster Job
(`deploy/pgbench-job.yaml`, co-located with the PoC Postgres by pod-affinity)
is the gate of record; the local workstation run (docker `postgres:18`) is for
iteration. PoC Postgres (user: `postgres:18`) runs as one pod in kind
(`deploy/postgres.yaml`, fixture `deploy/postgres-init.sql`): app role
`wamn_app` is `NOSUPERUSER`/`NOBYPASSRLS`, and every table has `FORCE ROW LEVEL
SECURITY` with policies keyed on `current_setting('app.tenant', true)`.

| Measure | In-cluster (kind pod, co-located) | Local (workstation) | Gate | Verdict |
|---|---|---|---|---|
| Throughput (1 component, 8-param single-statement, ≤10-row) | **20,427 qps** (16 workers) | 12,593 qps (24 workers) | ≥ 2,000 qps | **PASS** (~10×) |
| Latency p50 | 710 µs | 1.83 ms | — | — |
| Latency p90 | 1.13 ms | 2.46 ms | — | — |
| Latency p99 | **1.98 ms** | 3.47 ms | < 10 ms | **PASS** |
| Latency max | 23.0 ms | 22.4 ms | — | — |
| Pool saturation (96 concurrent 1 s queries, 16-conn pool) | 33 served, **63 `connection-unavailable`**, 0 hangs, worst 3.0 s | same shape | graceful, no hang | **PASS** |

**Security gates (all mandatory) — all PASS in-cluster:**

- **Chaos** (epoch-kill mid-transaction 100×): the guest `begin()`s a
  transaction, writes, then busy-loops; a per-store epoch deadline (the
  wamn-4p3 carried patch) traps it as `Trap::Interrupt`. On store teardown the
  `PgTransaction` `Drop` calls `deadpool Object::take` — removing the
  connection from pool accounting before closing it, so it can never be reused
  — and closing the socket makes the server abort the transaction. Result:
  100/100 interrupted, **100/100 connections destroyed**, 93 distinct fresh
  backend PIDs after the kills (pool churn observable), and **every** post-kill
  checkout was claim-free and transaction-free (see the empty-string note
  below).
- **RLS** (10,000 randomized cross-tenant attempts, two identities on one
  table): **0 rows leaked**; own-tenant sanity reads returned 1000/1000 each
  (RLS is scoping, not blanket-denying); **984/984 cross-tenant writes were
  `permission-denied`** (RLS `WITH CHECK` → the detail-free variant — no policy
  reconnaissance).
- **Injection** (10,000 param fragments incl. `'; DROP TABLE …`, `' OR '1'='1`,
  quotes, unicode, `$$`): **0 mismatches** — every fragment round-tripped
  byte-identically as data, and the scratch table was intact afterward. There
  is no interpolation code path; `$1..$n` binding is the only way data reaches
  the server.

**Notes on method / findings that feed downstream:**

1. **CFS throttling dominated the in-cluster p99 tail, not the plugin.** With a
   `cpu` *limit* on the Job container, in-cluster p99 was 31–44 ms (p50/p90
   stayed at 2/4 ms — the signature of quota exhaustion stalling the runtime
   for the rest of each 100 ms window). Removing the CPU limit (Burstable QoS,
   no quota) dropped p99 to 1.98 ms with no other change. **Operational
   consequence for 2.2 / D5:** the DB-serving path must not run under a tight
   CPU quota; size requests, don't cap. Cross-node placement added a smaller
   tail (kind overlay hop, 44→31 ms), removed by co-locating the workload with
   its pool — consistent with a node-local pooling topology.
2. **Empty-claim reset is safe.** Postgres reverts a custom GUC (`app.tenant`)
   to the empty string, not NULL, after a `SET LOCAL`, so an idle pooled
   connection reads back `Some("")`. That grants nothing — RLS compares
   `tenant_id = ''`, which no row satisfies — and every actual query runs
   inside a fresh `BEGIN; SET LOCAL app.tenant='<tenant>'`. The chaos gate's
   cleanliness check treats empty as claim-free; a non-empty residual would be
   a leak.
3. **D5 (pooling topology) input:** a per-host bounded pool returns
   `connection-unavailable` (a retryable variant) the moment demand exceeds
   capacity for longer than the checkout wait — it never hangs (63/96 excess
   requests failed fast within the 2 s wait timeout; worst call 3.0 s = one
   wait window plus a served 1 s query). This is the "graceful saturation"
   behavior D5 needs to reason about.
4. **Numeric fidelity (plan 3.3):** results decode in the binary wire format
   with a manual binary-NUMERIC → canonical-string decoder (unit-tested) so
   `numeric` values never touch `f64`; params travel in the text format so the
   server parses each against its declared column type, and `timestamptz`
   travels as an RFC-3339 string. No float coercion on the exact-decimal path.

**Fail branch:** not taken — the platform thesis (safe in-process DB
capability) holds. Closing S2 unblocks 2.2 (production plugin, wamn-ui3), D5
(wamn-qwd), S3 (wamn-lsf), and [P0-EXIT] (wamn-2rl).

## S3 — Flow-runner PoC (5.2) — pending

## S4 — Custom-node invocation + config parse (5.6, D7, note 9b) — pending

## S5 — Logging capture (9.3) — pending

## S6 — Test host plugin-swap (11.1) — pending

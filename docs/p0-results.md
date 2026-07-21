# P0 Spike Results — Raw Numbers

> **§1.9a audit (2026-07-19): amendments are additive — base sound.**

> **Provenance banner (E6, 2026-07-19):** fixture-pod measurements ran
> `fsync=off` + `synchronous_commit=off` — **shape-only, not citable
> externally** (wamn-dzhw tracks the durable-commit re-measure).

Records measurements against `docs/archive/p0-exit-criteria.md`. One section per spike;
the cross-cutting exit (decision closure for D5/D7/design-note-9b + the
dispatch-SLO sanity-check) is the final section below.

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
custom image values (`deploy/infra/values-wamn.yaml`); 3 host pods self-registered
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
   on a host, not per-workload-differentiable. **RESOLVED** by the fork's
   memory-limiter commit (wamn-bp4.1, `5b158ff`, D16): two-tier — the pooling
   cap is the platform *ceiling*, and a per-store `ResourceLimiter` enforces
   the per-component *linear-memory* budget (`memory_limit_mb` /
   `wamn.memory-limit-mb`) below it; budget > ceiling is a hard store-creation
   error; denials are logged + counted (`wamn::memory`). Gate: bench phase 5 —
   concurrent 64/192 MiB budgets under the 256 ceiling trapped at 56/184 MiB
   respectively, unbudgeted unchanged at 248, over-ceiling never allocated
   (docs/wash-runtime-fork.md).
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

**Build mechanics** *(historical — superseded 2026-07-12: wash-runtime is now
consumed as a git dep from the fork, the patch carried as a commit; see
`docs/wash-runtime-fork.md`)*: `scripts/vendor-wasmcloud.sh` cloned the pinned
monorepo rev into `vendor/wasmcloud` (gitignored) and applied
`patches/*.patch`; the root `Cargo.toml` `[patch]` section redirected the git
dep to that checkout (inside the real monorepo so `workspace = true` deps
resolve — `vendor` was excluded from our workspace for the same reason). The
Dockerfile ran the same script, so image builds were reproducible.
`patches/0002-workspace-lints-warn-not-deny.patch` relaxed the monorepo's
`-D warnings`: path deps don't get the `--cap-lints allow` that git deps get
(retired with the switch — the git dep gets it automatically).

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
(`deploy/gates/pgbench-job.yaml`, co-located with the PoC Postgres by pod-affinity)
is the gate of record; the local workstation run (docker `postgres:18`) is for
iteration. PoC Postgres (user: `postgres:18`) runs as one pod in kind
(`deploy/platform/postgres.yaml`, fixture `deploy/sql/postgres-init.sql`): app role
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

> **Addendum (2026-07-19, wamn-2jkm.41 rider 7 — re-gate after the R2/R16
> bound-`set_config` claim path `f7652c6` and the SR9 image split):** in-cluster
> **16,215 qps**, p50 **894 µs**, p90 1.43 ms, p99 **2.50 ms**, max 32.7 ms,
> 0 errors — thresholds unchanged, **PASS**. vs the row above: ~+26% p50
> (+184 µs) and p99 1.98→2.50 ms, the anticipated cost of the extra
> `set_config` round-trip on txn open (pipelined via `tokio::join!`). All
> security gates re-passed in the same run (chaos ×100, RLS ×10k, injection
> ×10k, in-band-override attack, multiproject pooling/policy). A first run
> concurrent with a full-workspace release compile on the host measured p99
> 14.7 ms — interference, discarded (the CFS/co-location lesson: measure quiet).

> **Addendum (2026-07-21, wamn-dzhw — DURABLE COMMITS):** the fixture pod
> flipped to `fsync=on` + `synchronous_commit=on` (all rows above were measured
> under `fsync=off` — systematically optimistic, see the ceilings.md provenance
> banner). Re-run under durable commits, in-cluster: **13,804 qps**, p50
> **998 µs**, p90 1.80 ms, p99 **3.59 ms**, max 34.6 ms, 0 errors — **PASS**
> (thresholds unchanged: qps ≥ 2,000, p99 < 10 ms). The durability cost vs the
> 2026-07-19 addendum: ≈15% qps, p99 +1.1 ms. Multiproject sibling: 14,162 qps,
> p99 3.06 ms, addressability 10,000/10,000. All security gates re-passed in
> the same runs. queuebench D15 dispatch SLOs also held durable: write-ahead
> p99 6.94 ms (< 15 ms), fast-path p99 6.06 ms (< 10 ms), doorbell warm p50
> 6.3 ms / p99 9.46 ms (< 25/100 ms). Fixture-pod figures from this date
> forward are durable-commit figures.

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
2. **Empty-claim reset is safe — now structurally, not by invariant.** Postgres
   reverts a custom GUC (`app.tenant`) to the empty string, not NULL, after a
   `SET LOCAL`, so an idle pooled connection reads back `Some("")`. Every actual
   query runs inside a fresh `BEGIN; SET LOCAL app.tenant='<tenant>'`. In P0 the
   empty claim granted nothing only because no row ever carried `tenant_id = ''`
   — an invariant nothing enforced (a superuser / BYPASSRLS write path could
   have landed such a row that an idle claimless connection would then see).
   Since **wamn-a45** the tenant floor makes this structural: the policy reads
   `NULLIF(current_setting('app.tenant', true), '')` (an empty claim folds to
   NULL and matches no row, including a hypothetical `''`-tenant row) and
   `CHECK (tenant_id <> '')` forbids a `''`-tenant row from existing at all.
   Both halves are proven independently by a live-apply gate
   (`empty_tenant_claim_matches_no_row_on_postgres`, `cargo test -p wamn-ddl`
   with `WAMN_DDL_PG_URL`) and mirrored across the emitter and the five
   hand-written run schemas. The chaos gate's cleanliness check still treats
   empty as claim-free; a non-empty residual would be a leak.
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

## S3 — Flow-runner PoC (5.2) — **PASS** (2026-07-10)

**Deliverable shipped:** a guest flow-runner (`components/flowrunner`) that
embeds the standard node library as **native Rust** and imports
`wamn:postgres/client`, plus a `wamn-host flowbench` subcommand
(`crates/wamn-host/src/flowbench.rs`) that drives it. The runner *is* a
long-lived component; the standard nodes are compiled in, so dispatching one is
an ordinary same-binary function call (`std_node`) — that is the `< 50 µs`
overhead the dispatch gate measures. Everything durable — the flow IR, the
run-state checkpoints, and the business sink — flows through the S2
`wamn:postgres` plugin under the host-injected tenant claim; the runner has no
other data path. The 5-node PoC graph is `webhook-in → transform → pg-write →
conditional → respond` (webhook-in/respond modeled as the walk's input/return —
the HTTP hop is S4). Flow JSON is a minimal versioned IR stored in a catalog
table; "deploy" flips the active-version pointer.

**Method:** like `pgbench`, `flowbench` instantiates the guest into a hand-built
`SharedCtx` store with the plugin linked and drives its exports. Fixture tables
`s3.flows` (versioned `graph_json` + active pointer), `s3.flow_runs`
(checkpoints; `step_seq` = highest completed step), and `s3.sink` (business
side effect, `UNIQUE (tenant_id, run_id, step)` idempotency key) live in
`deploy/sql/postgres-init.sql` under the same `FORCE ROW LEVEL SECURITY` shape as
s2. The in-cluster Job (`deploy/gates/flowbench-job.yaml`, co-located, no CPU limit —
the S2 CFS lesson) is the gate of record; the local docker run is for iteration.

| Gate | In-cluster (kind pod, co-located) | Local (workstation) | Threshold | Verdict |
|---|---|---|---|---|
| Standard-node dispatch p99 (same-binary) | **0.83 µs** (mean 120 ns/dispatch, max 64.9 µs) | 0.80 µs (mean 124 ns) | < 50 µs | **PASS** (~60×) |
| Hot-reload flip → version live | **428 µs** worst (5 flips) | 2.64 ms | < 1 s | **PASS** (~2000×) |
| Kill-mid-run resume, side-effect rows | **10/10 exactly one row**, 10/10 duplicate-absorbed | same | exactly 1 | **PASS** |

**How each gate is constructed:**

- **Dispatch** (`dispatch-bench`): the runner walks the 5-node graph
  `iterations` times entirely in-component, with the pg-write side effect
  stubbed to a counter — no DB, no host boundary crossed per node. An
  un-instrumented pass gives the amortized mean; an instrumented pass times each
  per-node dispatch with the monotonic clock (each sample therefore *includes*
  one clock read, so p50/p99/max are conservative upper bounds on the true
  dispatch cost). The gate isolates same-binary dispatch from both I/O and the
  wasm boundary, which would otherwise dwarf a sub-µs signal. (The lone `max`
  outlier — tens of µs — is a single scheduler preemption; the gate is on p99.)
- **Hot-reload**: the harness flips `s3.flows.active` between v1 (upper-cases the
  payload) and v2 (reverses it), then re-reads the active version until the flip
  is observed and confirms a fresh run now executes the *new* version's behavior
  — proving the flip changes real execution, not just a pointer. The PoC
  observes the flip by catalog re-read; the production doorbell is NATS core
  (wamn-m2z [5.14]).
- **Resume / idempotency**: a runner runs into the kill window — it commits the
  pg-write side effect, then busy-loops *before* writing its checkpoint (the
  exact pod-death window) and is epoch-killed (`Trap::Interrupt`, the wamn-4p3
  patch). A fresh instance resumes from the last checkpoint; because the sink
  write replays under the same `(run_id, step)` key, `ON CONFLICT DO NOTHING`
  absorbs the duplicate. Every cycle: clean epoch trap, the side effect was
  committed pre-kill (so the resume faced a *genuine* duplicate), and the run
  ended with **exactly one** sink row — never two.

**Notes on method / findings that feed downstream:**

1. **Dispatch is not topology-sensitive (unlike S2's p99).** The dispatch metric
   is a same-binary, DB-free CPU measurement, so local and in-cluster agree
   (0.80 vs 0.83 µs); the Job still runs with no CPU limit to keep a CFS quota
   from injecting scheduler stalls into the tail. The ~60× headroom (sub-µs vs
   50 µs) confirms the architecture thesis: standard nodes compiled into the
   runner cost nothing to dispatch, so the per-node budget belongs to real work,
   not the framework.
2. **Dispatch-SLO sanity vs 5.14 (for [P0-EXIT]).** 5.14 proposes platform-
   overhead SLOs of sync write-ahead p99 < 15 ms, fast-path < 10 ms, async warm
   p50 < 25 ms. S3 shows the *graph-walk* contribution to those budgets is
   negligible (sub-µs per node); the budgets are dominated by the S2 DB round
   trip (p99 1.98 ms in-cluster) and the trigger/queue path, not node dispatch.
   The proposed SLOs remain internally consistent with the measured S2+S3
   numbers.
3. **PoC shortcuts and where the real work is tracked** (per the S3 decision to
   confirm scope up front): catalog re-read stands in for the NATS doorbell →
   wamn-m2z [5.14]; the minimal ad-hoc flow JSON → wamn-34t [5.1] (canonical
   schema); webhook-in/respond as walk input/return → trigger dispatch in
   wamn-m2z [5.14] + the production runner wamn-uyd [5.2] (sync webhook path is
   locked as D15). The runner also *writes* the catalog (seed/flip) for the PoC;
   in production that is a control-plane action and the runner only reads.

**Fail branch:** not taken — high dispatch overhead would have forced a plan-
representation rethink; resume failure would have reworked checkpoint
granularity. Both held. Closing S3 unblocks S4 (wamn-veg), S6 (wamn-jy9),
[P0-EXIT] (wamn-2rl), and the production flow-runner 5.2 (wamn-uyd).

## S4 — Custom-node invocation + config parse (5.6, D7, note 9b) — **PASS** (2026-07-10)

**Deliverable shipped:** one custom node in **two guest languages** implementing
the minimal `wamn:node` contract (docs/wamn-node.wit) — `components/node-rs`
(Rust) and `components/node-ts` (TypeScript/JS via **JCO** / ComponentizeJS /
StarlingMonkey) — plus a **`wac`-composed** frozen 3-node flow
(`components/flow-driver` + node-rs → `flow-composed.wasm`), driven by a new
`wamn-host nodebench` subcommand (`crates/wamn-host/src/nodebench.rs`) and a
`serve-node` HTTP node host. The node has three config-selected modes: `noop`
(hop), `io` (a host `wait-ns` sleep modelling an outbound call), and `compute`
(a bounded FNV-1a loop). Both guests call the **same** host `wait-ns` import, so
the I/O floor is byte-identical across languages and the interpreted-vs-composed
gap on the I/O-bound flow is pure framework overhead (the production outbound
path is wasi:http, 5.6 / wamn-bd5).

**Method:** the hop gate runs a real HTTP/1.1 round trip to a warm Rust `noop`
node — in-cluster it targets a separate `serve-node` **pod** through a Service
(`deploy/gates/serve-node.yaml`), so it is a true cross-pod hop; locally it is an
in-process loopback server. The gap and config gates run in-process (no HTTP
noise): the harness instantiates each guest into a hand-built `SharedCtx` store
with `wasi` + the `wait-ns` import linked (the flowbench/pgbench pattern, here
using `bindgen!` for the typed `handler.run(ctx, input)`), and times the 3-node
flow three ways — JS-dynamic (interpreted), Rust-dynamic, and Rust-composed
(the `wac` artifact). The in-cluster Job (`deploy/gates/nodebench-job.yaml`, no CPU
limit — the S2 CFS lesson) is the gate of record.

| Gate | In-cluster (kind) | Local (workstation) | Threshold | Verdict |
|---|---|---|---|---|
| HTTP hop p50 (D7) | **33 µs** (p99 89 µs), cross-pod | 37 µs (p99 104 µs) loopback | < 2 ms | **PASS** |
| Interpreted-vs-composed gap, **I/O-bound** | **+2.8%** (composed 78.9 ms, JS 81.1 ms) | +2.6% (78.8 / 80.9 ms) | < 5% | **PASS** |
| Interpreted-vs-composed gap, **compute-bound** | +27601% (~277×; 0.87 / 241 ms) | +51726% (~518×; 0.87 / 449 ms) | large *expected* | **as expected** |
| Config-JSON-parse share of cold dispatch (9b) | 5.90% (parse 1156 ns / cold 19 µs) | 5.82% (1653 ns / 28 µs) | ≤ 5% | **decision (below)** |

**How each gate is constructed:**

- **HTTP hop (D7)**: 2000 sequential POST /run round trips on one keep-alive
  connection to a warm `noop` node. With ~0 node compute the round trip *is* the
  invoke overhead (invoke − compute ≈ invoke). In-cluster the client and the
  `serve-node` server are separate pods reached through a ClusterIP Service, so
  the number includes real pod-network + HTTP/1.1 framing + payload
  (de)serialization + the wasm call.
- **Interpreted-vs-composed gap**: the 3-node flow's total latency, warm, three
  ways. The **I/O-bound** flow waits 25 ms/hop (a realistic outbound DB/API
  call) so the interpreter's fixed per-invocation cost is a small fraction; the
  **compute-bound** flow runs a CPU hashing loop where the JS interpreter is
  hundreds of × slower than native. Gap = (interpreted − composed) / composed.
- **Config-parse (9b)**: cold instantiate + first run of the Rust node, which
  self-times its `serde_json` config parse (`parse_ns`) against the harness-timed
  cold dispatch. The denominator is the *tightest honest* cold dispatch — a
  pooled instantiate + one invoke of an already-compiled component (~19 µs) — so
  the share is a conservative upper bound.

**Findings / decisions (feed [P0-EXIT] wamn-2rl):**

1. **D7 confirmed — in-cluster HTTP is the v0 invocation path.** The cross-pod
   hop p50 is ~33 µs, ~60× under the 2 ms gate and far under the 5 ms escalation
   line, so the component-linking / wRPC spike stays a *later* optimization, not
   P1.
2. **Interpreter default confirmed for I/O-bound flows.** On a realistically
   sized I/O-bound flow the JS/JCO interpreter costs only ~2.8% more than a
   `wac`-composed native flow — under the 5% gate. Since the vast majority of
   real nodes are I/O-bound (API/DB calls), defaulting to the interpreted
   authoring path is sound.
3. **Frozen flows' post-GA slot sized.** On a compute-bound flow the interpreter
   is a few hundred × slower than native — a large gap, *expected*, and exactly
   the case `wac`-composed frozen flows (5.13) address. Not a gate; it confirms
   frozen flows earn their post-GA slot for compute-heavy flows only. (The exact
   multiple varies with CPU/JIT warmth — 277× in-cluster, 518× on the
   workstation — but the order of magnitude is the point.)
4. **Composition ≈ dynamic-native; the interpreter is the axis.** Rust-composed
   and Rust-dynamic were within noise (±1%) on both workloads: `wac` composition
   does not itself cut steady-state latency at these scales (its wins are single
   instantiation + no dynamic dispatch + config constant-folding). The
   language/authoring choice, not composition, dominates the gap.
5. **Design-note 9b — decision: keep the mitigation.** Config parse is ~6% of
   the tightest cold dispatch (pooled instantiate + first run, no compile) —
   marginally *above* the 5% line. In absolute terms it is ~1.2 µs, and against a
   realistic cold start (component fetch + JIT compile) it is «1%. So the
   practical exposure is bounded, and note 9b's planned mitigation — memoize the
   parse per `(flow-version, node-id)` and constant-fold config in frozen flows
   — is **confirmed warranted and retained**, not dropped. Standard-library
   nodes compiled into the runner never touch the JSON codec at all (S3), so
   this cost is scoped to dynamically-loaded custom nodes.

**Method notes / PoC shortcuts and where the real work is tracked:** I/O is a
host `wait-ns` sleep, not real wasi:http — the decision rule only needs I/O to
dominate; production outbound HTTP is the runner's job (5.6 / wamn-uyd,
wamn-bd5). The tokio-timer granularity (~1 ms) floors any single wait, so the
I/O-bound flow is sized at 25 ms/hop where that floor is negligible. The
composed flow still passes config as JSON per hop (the inner node parses it), so
the gap measures composition + single-instantiation, not config constant-
folding; true frozen-flow constant-folding (5.13) removes even that, bounded by
the 9b number above. StarlingMonkey's compute cost reflects the interpreter
without a warmed JIT; a production JS runtime with JIT would narrow the
compute-bound gap but not change the I/O-bound conclusion.

**Fail branch:** not taken — hop p50 > 5 ms would have pulled the
component-linking/wRPC spike into P1; a > 5% I/O-bound gap would have dropped the
interpreter default. Both held. Closing S4 unblocks the production custom-node
work 5.6 (wamn-bd5) and feeds [P0-EXIT] wamn-2rl (D7 + note 9b now closable with
data).

## S5 — Logging capture (9.3) — **PASS** (2026-07-10)

**Deliverable shipped:** a custom **`wamn:logging` host plugin**
(`crates/wamn-host/src/plugins/wamn_logging.rs`) implementing `wasi:logging/logging`
as the platform's log-capture path, plus a guest fixture
(`components/logspewer`) and a `wamn-host logbench` subcommand
(`crates/wamn-host/src/logbench.rs`) that drives it against a real **OTel
Collector → Loki** pipeline (`deploy/infra/otel-collector.yaml`, `deploy/infra/loki.yaml`).
The plugin replaces the vendored `TracingLogger`: it **enriches** every record
with host-trusted `tenant`/`project` (from a component→claim map — a guest can
*not* spoof its tenant) plus `flow`/`run`/`node` parsed from the guest's
`context`, ships them as structured OTel log attributes, and **owns a bounded
front queue + atomic drop counter** so a `log()` call is non-blocking and any
rate-limit drop is *counted*, never silent.

**Method:** the plugin owns its OWN `SdkLoggerProvider` (a generously sized
batch processor → OTLP/gRPC → collector), independent of the vendored
`observability.rs` logs pipeline whose fixed 2048-entry batch queue and
`--log-level`-tied filter would bottleneck/misfilter a 10k lines/s bench. A
collector is *required* between host and Loki (the host exporter is gRPC/tonic;
Loki's OTLP ingest is HTTP) — it receives OTLP/gRPC and forwards to Loki's
native `/otlp/v1/logs` over `otlphttp`. Loki promotes `service.name` to the
`service_name` label and keeps the enrichment fields as structured metadata, so
loss/enrichment are measured by exact LogQL counts
(`sum(count_over_time({service_name="wamn-host"} | run_label="…" [Δ]))`), with
the collector's internal `otelcol_exporter_sent_log_records` as a cross-check.
The in-cluster Job (`deploy/gates/logbench-job.yaml`, no CPU limit — the S2 CFS
lesson) is the gate of record; local docker is for iteration.

| Gate | In-cluster (kind) | Local (workstation) | Threshold | Verdict |
|---|---|---|---|---|
| Per-call `log()` overhead, guest-observed | **p99 5.78 µs** (max 12.5 µs, p50 1.6 µs) | p99 11.6 µs (max 28.3 µs, p50 4.3 µs) | < 50 µs | **PASS** |
| Loss at 10k lines/s × 30s (300k lines) | **0 unaccounted / 300000 (0.0000%)**; delivered 300000, dropped 0 | 0 / 300000 (0.0000%) | < 0.1% unaccounted | **PASS** |
| Rate-limit drops **counted, not silent** | **195699 dropped (visible)**, 0 unaccounted on a 200k burst | 195724 dropped, 0 unaccounted | drops > 0 & counted | **PASS** |
| Enrichment (tenant/project/flow/run/node on every record) | **5000/5000 (100%)** | 5000/5000 (100%) | 100% | **PASS** |

**How each gate is constructed:**

- **Overhead (<50 µs):** the guest emits N `log()` calls, self-timing *each*
  with `std::time::Instant` (works on wasm32-wasip2 — S3/S4), and returns the
  per-call nanoseconds. Because the plugin's `log()` only enriches + `try_send`s
  onto the front queue (the OTLP export is a background drain task), this is the
  boundary + enrich + enqueue cost, not the export.
- **Loss (<0.1% unaccounted):** the harness paces the guest to 10k lines/s for
  30s (300k lines), each line carrying a unique `seq` + a per-run `run_label`.
  After the front queue drains and the batch processor flushes, it queries Loki
  for the exact delivered count and accounts
  `unaccounted = emitted − delivered − dropped`, gating `unaccounted/emitted <
  0.1%`. Everything downstream of the plugin's front queue is sized not to drop,
  so the only intentional drop point is counted. In-cluster all 300000 lines
  were delivered (0 lost, 0 dropped); the collector's cumulative
  `sent_log_records` (302200 = 300000 + the 2200 overhead-phase calls)
  independently corroborates the Loki count.
- **Drops counted (not silent):** a saturation burst (200k lines) into a small
  bounded queue (4096) draining at a throttled rate overflows the queue on
  purpose; the plugin's atomic drop counter (also surfaced as OTel metric
  `wamn.logging.dropped`) records every drop (195699 in-cluster), and the
  accounting still closes exactly (`delivered + dropped = emitted`, 0
  unaccounted).
- **Enrichment (100%):** a Loki count of the run filtered to require all five
  fields non-empty (`| tenant!="" | project!="" | …`) must equal the unfiltered
  count — i.e. every delivered record is fully enriched (5000/5000).

**Findings / decisions (feed [P0-EXIT] wamn-2rl):**

1. **Logging capture is viable at v0 — no P1 buffer/agent redesign.** The
   capture path sustains the target rate with 0 unaccounted loss and 100%
   enrichment, so the S5 fail branch (logging becomes a P1 workstream) is **not
   taken**. 9.3 production (wamn-yf3) proceeds on this shape.
2. **Enrichment must be host-owned.** tenant/project are injected from a
   host-trusted claim map (like the S2 `wamn:postgres` tenant claim), never from
   the guest — a guest cannot forge its tenant. flow/run/node come from the
   runner via the `context` string. This is the same identity S2/S3 already
   plumb, so run history and logs share one enrichment source.
3. **A collector is structurally required.** Host OTLP is gRPC/tonic; Loki OTLP
   is HTTP. The OTel Collector is the bridge (and its internal metrics are a
   convenient loss cross-check). This is a fixed piece of the 9.3 topology.
4. **Drops are a first-class, counted signal.** The plugin's bounded front queue
   is the single intentional drop point; overflow increments an atomic counter
   surfaced as a metric. Rate-limiting is therefore observable, not silent —
   satisfying the S5 requirement and giving 9.3 a back-pressure signal to alarm
   on.

**Method notes / PoC shortcuts and where the real work is tracked:** the guest
`context` is a small JSON object the plugin parses for flow/run/node; the
production runner supplies these on the real invocation context (5.2 path). The
plugin owns a dedicated `SdkLoggerProvider` for the PoC to control batch sizing;
9.3 production (wamn-yf3) folds this into the host's observability wiring
(sizing the batch config from config rather than a second provider). Loki is a
single-binary filesystem instance sized for the bench; production sink topology
(retention, multitenancy, HA) is 9.x / control-plane work. A benign teardown log
(`failed to shutdown meter provider`) prints after the PASS lines — it is the
vendored `observability.rs` meter-provider shutdown racing process exit, not a
capture-path fault.

**Fail branch:** not taken — >0.1% unaccounted loss or <100% enrichment would
have made logging a P1 workstream with a buffer/agent redesign (run history
depends on the same enrichment). Both held. Closing S5 unblocks the production
logging plugin 9.3 (wamn-yf3) and feeds [P0-EXIT] wamn-2rl.

## S6 — Test host plugin-swap (11.1) — **PASS** (2026-07-10)

**Deliverable shipped:** the S3 flow-runner (`components/flowrunner`) extended
with two nodes that touch *non-deterministic* host capabilities — a **`delay`**
node (reads `wasi:clocks/wall-clock`, parks durably) and an **`http-call`** node
(makes a `wasi:http/outgoing-handler` outbound request) — plus a `wamn-host
testhostbench` subcommand (`crates/wamn-host/src/testhostbench.rs`) that compiles
the extended runner **once** and instantiates the *identical bytes* into two
stores that differ only in host-injected capabilities:

- **PROD store** — real wall clock (default `WasiCtx`), a forward-all egress
  handler, and `wamn:postgres` pointed at the shared fixture schema `s3`.
- **TEST store** — a *virtual* wall clock the harness advances (via
  `CtxBuilder::with_wasi_ctx`), an egress **spy** that records and denies
  unexpected outbound calls (via `with_http_handler`), and `wamn:postgres`
  pointed at a fresh **ephemeral schema** provisioned from the template DDL.

The swap is a per-store wiring change; the guest is byte-identical. This is the
mock-at-capability-boundary thesis (design-note 9): time enters the flow only
through `wasi:clocks`, egress only through `wasi:http`, and durable state only
through `wamn:postgres` — so the test host virtualizes each at the boundary with
zero node changes. To make the schema itself a host-swapped fixture, the runner
uses **unqualified** table names and each host injects the schema via
`SET LOCAL search_path` (a new, optional `wamn:postgres` capability alongside the
tenant claim) — prod → `s3`, test → the ephemeral schema.

**Method:** the test host provisions the ephemeral schema through a **superuser**
connection — the runner's `wamn_app` role is `NOSUPERUSER/NOCREATEDB` and cannot
create schemas, exactly as in production; prod and test use **separate** pools so
each pool's `search_path` is stable and prepared-statement plans never alias
schemas. The 24h delay uses **parked-wake**: the delay node records a wake
deadline (`now() + 24h`) in the run's checkpoint and returns *parked*; the
harness advances the virtual clock past the deadline and re-runs, so the run
completes — reusing the S3 checkpoint/resume machinery and testing the real
durable-flow mechanism, not a test-only shortcut. The egress spy delegates
*expected* calls to the same `DefaultOutgoingHandler` production uses (a real
send to a loopback echo) and *denies* anything whose authority is not on the
expectation list. The in-cluster Job (`deploy/gates/testhostbench-job.yaml`, no CPU
limit — the S2 CFS lesson) is the gate of record; local docker is for iteration.

| Gate | In-cluster (kind) | Local (workstation) | Threshold | Verdict |
|---|---|---|---|---|
| Same binary runs under both host wirings | one component (fnv1a `0x196d7ec0f22a453a`) → prod + test, both complete, 1 sink row each | same digest, both complete | zero component changes | **PASS** |
| 24h-delay flow completes < 1s wall under virtual time | 4.06 ms wall (parked → advance clock +86401s → completed); prod real clock stays parked | 31.6 ms wall; prod stays parked | < 1s | **PASS** |
| Egress spy catches an intentionally-added unexpected call | planted `http://169.254.169.254/...` flagged + denied (http 0); expected echo call forwarded (200), not flagged | identical | planted call caught | **PASS** |
| S3 regression on the extended binary (dispatch / hot-reload / resume) | dispatch p99 0.95 µs; hot-reload worst 741 µs; resume 10/10 single idempotent row | dispatch p99 0.91 µs; hot-reload 2.30 ms; resume 10/10 | S3 thresholds hold | **PASS** |

**How each gate is constructed:**

- **Sameness:** the harness compiles the component once into a single
  `InstancePre` and instantiates it into both stores; a zero-delay delay+http
  flow runs to completion under each, writing exactly one sink row. The
  reported fnv1a digest is the same bytes both stores loaded — "no source diff"
  is proven as "identical bytes," not merely an unchanged file.
- **Delay (< 1s under virtual time):** the test store seeds a flow with a
  24h delay node, runs it once (parks), advances the shared virtual clock past
  the deadline, and runs again (completes) — all in real milliseconds. The prod
  store runs the *same* flow under its real clock and stays parked (we do not
  wait 24h), proving the delay is genuine and only virtual time collapses it.
- **Egress spy (catch the planted call):** the test store's spy expects only the
  loopback echo's authority. An expected call is recorded and forwarded (200); a
  planted call to a link-local cloud-metadata endpoint (a classic SSRF target)
  is flagged and denied without ever leaving the host (the guest observes
  status 0). The expected/planted split is the "intentionally-added unexpected
  outbound call."
- **Regression:** the harness re-runs the full S3 flowbench (dispatch /
  hot-reload / resume) against the *extended* binary, proving the added nodes and
  the schema-via-search_path change did not regress S3.

**Findings / decisions (feed [P0-EXIT] wamn-2rl):**

1. **The mock-at-capability-boundary thesis holds — no design-note 9 rework.**
   The identical flow binary runs unmodified under both hosts; time, egress, and
   the durable store are each swappable at the boundary. The S6 fail branch (a
   leaked ambient capability forcing determinism-rule changes before Epic 11) is
   **not taken**.
2. **Schema is a host-swapped fixture, like the tenant claim.** Making the
   runner schema-agnostic (unqualified names + host-injected `search_path`) is
   what lets the test host give each run a clean ephemeral schema with zero node
   changes. This generalizes: the flow names *what* it wants (a table), the host
   decides *where* it lives.
3. **Parked-wake is the right durable-delay primitive.** Because the delay node
   reads `wasi:clocks` and checkpoints rather than sleeping in-guest, the test
   host virtualizes only `now()` — no timer/poll-subsystem virtualization — and
   the same mechanism is what production uses for long waits (POC-F3 cron).
4. **Egress is a host chokepoint.** Routing all outbound HTTP through one
   `HostHandler` gives the platform a single point to record, allow-list, and
   deny egress — the test spy and a production egress policy are the same seam.

**Method notes / PoC shortcuts and where the real work is tracked:** the flow IR
is the same minimal ad-hoc JSON as S3 (canonical schema → wamn-34t / 5.1); the
ephemeral schema is created per testhostbench invocation (stable per pool for
prepared-statement safety), which is the natural granularity — per-flow-run
isolation is a production test-runner concern (11.x). The egress spy denies by
authority allow-list; the production egress policy (per-tenant allowed-hosts,
audit) is 8.x / control-plane work. The `wamn.schema` host-injected config key is
wired end-to-end but only exercised by the bench here; 11.1 production consumes
it from the test WorkloadDeployment.

**Fail branch:** not taken — a binary that needed *any* change between hosts, a
delay that could not be collapsed by virtual time, or an egress that escaped the
spy would have weakened the thesis and forced a design-note 9 revision before
Epic 11. All held. Closing S6 completes the P0 spike set (S1–S6) and unblocks
[P0-EXIT] wamn-2rl (the last open P0 spike) and the 11.1 production test host.

## Cross-cutting exit — P0 COMPLETE (2026-07-10)

All six P0 spikes passed on their in-cluster gate of record; none took its fail
branch. Per `docs/archive/p0-exit-criteria.md` ("Cross-cutting exit"), P0 is done when
every spike passes (or has a written fail-branch decision), the raw numbers are
recorded here, and the data-dependent decisions (D5, D7, design-note 9b, and the
proposed dispatch SLOs) are formally closable with that data. All conditions are
met.

### Six-spike verdict

| Spike | What it gated | bd issue | Headline result (in-cluster) | Verdict |
|---|---|---|---|---|
| S1 | Custom host image | wamn-u1u | instantiation p99 < 10 ms; stable @100 components; clean 256 MiB cap kill | **PASS · closed** |
| S2 | `wamn:postgres` plugin + security | wamn-czh | 20427 qps, p99 1.98 ms; chaos/RLS/injection gates all clean | **PASS · closed** |
| S3 | Flow-runner (dispatch / reload / resume) | wamn-lsf | dispatch p99 0.83 µs; hot-reload < 1 s; idempotent resume 10/10 | **PASS · closed** |
| S4 | Custom-node invocation + config parse | wamn-veg | hop p50 33 µs; I/O interpreted-vs-composed gap +2.8%; parse ~6% of cold | **PASS · closed** |
| S5 | Logging capture | wamn-7gc | log() p99 5.78 µs; 0 unaccounted @10k lines/s; enrichment 100% | **PASS · closed** |
| S6 | Test host plugin-swap | wamn-jy9 | same binary both hosts; 24h delay 4.06 ms wall; egress spy catches planted call | **PASS · closed** |

No fail branch was taken. The S2 security gates (chaos / RLS / injection — the
mandatory stop-the-line) all passed clean.

### Decision closures

**D5 — Postgres pooling topology → Hybrid (per-host now, pgBouncer later).**
S2 shipped a deadpool per-host pool that sustained 20427 qps at p99 1.98 ms and
degraded to graceful `connection-unavailable` under saturation. For v0/P1 that
per-host cap is the pooling answer; pgBouncer **transaction-mode** pooling is
added when M×N connection pressure (N host pods × M project DBs) appears against
server `max_connections`. Transaction-mode is safe here because D4 removed
LISTEN/NOTIFY entirely (no session-scoped listener) and claims are injected via
`SET LOCAL`, which resets at COMMIT (session-mode pooling would leak the GUCs).
The S2 CFS lesson stands: the DB-serving path must not be CPU-quota-capped.
(bd wamn-qwd closed.)

**D7 — Custom-node invocation → In-cluster HTTP, CONFIRMED.** S4 measured an
in-cluster cross-pod HTTP hop at p50 33 µs / p99 89 µs — orders of magnitude
under the 2 ms escalation threshold. No move to wasmCloud component linking /
NATS wRPC; the post-P0 revisit is dropped.

**Design-note 9b — config parse cost → keep memoize + constant-fold (benign).**
S4 measured config JSON parse at ~6% of the *tightest* cold dispatch (~1.2 µs
parse vs ~19 µs pooled instantiate+run) — marginally over the 5% gate line. The
existing mitigation (memoize per `(flow-version, node-id)` in warm instances;
`wac` constant-fold in frozen flows) stays; exposure is bounded and the share is
≪1% once component load/compile is counted. Strict schema-validated JSON is
retained. (docs/wamn-node-design-notes.md note 9b updated.)

**Dispatch SLOs (5.14 / D15) — sanity-checked, consistent, > 5× headroom.** The
proposed platform-overhead SLOs sanity-check cleanly against measured spike
latencies:

| Proposed SLO (5.14) | Target | Measured budget line | Headroom |
|---|---|---|---|
| Sync write-ahead p99 | < 15 ms | 1 durable INSERT (S2 p99 1.98 ms) + native node dispatch (S3 µs) | > 7× |
| Sync fast-path p99 | < 10 ms | reduced-audit: node dispatch + optional pg ops | > 5× |
| Async warm p50 / p99 | < 25 ms / < 100 ms | doorbell + INSERT + dispatch, all sub-ms | ≫ |
| Async cold (parked wake) p99 | < 250 ms | wake + resume replay (S3/S6 mechanism) | ≫ |

Since P0, the 5.14 `queuebench` in-cluster gate of record has **measured** the
dispatch path directly, and the numbers clear the proposed SLOs with wide margin:
sync write-ahead **p99 1.11 ms** (vs < 15 ms proposed), reduced-audit fast-path
**361 µs** (vs < 10 ms), and the async warm doorbell **300 / 300 delivered** at
**p50 < 25 ms / p99 < 100 ms**. This records the data that makes the flag
signable; it does not itself sign off.

D15 stays **Locked; SLO numbers proposed** — the sanity-check is recorded here;
the formal product sign-off remains a separate call and is deliberately left
pending.

### Exit statement

**P0 exit criteria are met (2026-07-10).** All six spikes passed on their gate of
record, raw numbers are recorded above, and every data-dependent decision (D5
chosen, D7 confirmed, design-note 9b recorded, dispatch SLOs sanity-checked) is
closed. The remaining open decisions (D6 hosting, D8 raw-SQL policy, D11/D12 MQTT
tranche) are downstream of P0 and out of this scope. P0 is complete; P1
production work is unblocked.

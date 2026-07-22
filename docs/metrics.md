# Metrics — the OTel metric set (9.8)

Host-native OTel metrics → OTel Collector → **Prometheus text on `:8889`**, so the
run/queue/pool/memory/API health of the platform is scrapeable beside the traces
(9.1, `docs/tracing.md`) and logs (S5). This is the metrics third of Epic 9
(`docs/platform-plan.md` §9.8).

- **Issue:** wamn-jn6 `[9.8]`; **Epic:** wamn-7ok (Observability).
- **Builds on:** 9.1's collector + the same activation switch (`OTEL_*` present).

## What is already free

The **meter provider is not new.** wash-runtime's `initialize_observability`
(the fork's `observability.rs`, called by every `wamn-host`/`wamn-gates`/
`wamn-run-worker` entry point) builds an OTLP `MetricExporter` →
`SdkMeterProvider` → `opentelemetry::global::set_meter_provider`, gated on
`OTEL_*` env presence (the same switch traces/logs use). So any instrument on
`opentelemetry::global::meter(...)` in the process exports over OTLP-gRPC when
`OTEL_*` is set — 9.8 **reuses** that global provider (the S5 lesson about a
separate *logs* provider does not apply to metrics; there is no queue/filter
bottleneck). The one exception is the **dispatcher**, the sole service artifact
that links no runtime (SR9): it builds its own minimal, `OTEL_*`-gated
`SdkMeterProvider` (`crates/wamn-dispatcher/src/main.rs`, `init_metrics`).

The fork already emitted `fuel.consumption`; 9.8's `wamn.api.requests` counter
rides beside it in the fork (see below).

## The metric set

No metric carries a `run_id` (unbounded cardinality). tenant/project reuse the
same host claim maps that enrich the 9.1 spans / S5 logs (a guest cannot spoof
them). Dots become underscores in the Prometheus scrape
(`wamn.run.executions` → `wamn_run_executions`); the collector's Prometheus
exporter runs with `add_metric_suffixes: false`, so no `_total`/unit suffixes are
added (histograms keep their structural `_count`/`_sum`/`_bucket`).

| Metric | Kind | Attributes | Emitted by (file) |
|---|---|---|---|
| `wamn.run.executions` | counter | `outcome` (completed/parked/failed), `wamn.tenant`, `wamn.project` | run-worker `drain` (`crates/wamn-run-worker/src/lib.rs`) |
| `wamn.run.drive.duration_ms` | histogram | `wamn.tenant`, `wamn.project` | run-worker `drain` (timed around `call_run_next`) |
| `wamn.run_queue.depth` | observable gauge (i64) | `wamn.tenant`, `wamn.project` | dispatcher `tick_project` (`crates/wamn-dispatcher/src/lib.rs`, `RUN_QUEUE_DEPTH_SQL`) |
| `wamn.postgres.pool.{size,available,waiting}` | observable gauge (u64) | `wamn.project` | `WamnPostgres::register_pool_metrics` (deadpool `Pool::status()`) |
| `wamn.postgres.query.duration_ms` | histogram | `db.operation` (query/execute/txn.query/txn.execute), `wamn.project` | the `db_span` sites (`crates/wamn-host/src/plugins/wamn_postgres/resources.rs`) |
| `wamn.memory.denied` | observable counter (u64) | `component` | `MemoryMeter` (`crates/wamn-host/src/memory_metrics.rs`) |
| `wamn.memory.high_water_bytes` | observable gauge (u64) | `component` | `MemoryMeter` |
| `wamn.memory.budget_bytes` | observable gauge (u64) | `component` | `MemoryMeter` (budgeted stores only) |
| `wamn.api.requests` | counter (u64) | `status_class` (2xx/4xx/5xx/…) | **fork** `record_response_status` (`host/http.rs`) |

### Emission notes

- **Run executions + success ratio.** The run-worker records one execution per
  claimed drive with the terminal `outcome`; the success ratio is
  `completed / (completed+failed)` computed at query time. The `outcome` fold
  (0→completed, 1→parked, else→failed) is the SAME one `DrainReport` uses.
- **Run-drive duration.** Whole-run drive time (around the guest `run-next`
  call). True **per-node** p50/p99 is guest-side (node_runs timestamps), like
  `node_id` on the 9.1 spans — **deferred** (see below), derivable by query.
- **Run-queue depth.** The dispatcher republishes each project's *claimable*
  depth every sweep (no new loop) — the count uses the EXACT claim predicate of
  `wamn_run_queue::claim_batch_sql` (available_at reached, lease NULL-or-expired,
  budget-remaining), so the gauge counts precisely what a runner could claim now.
- **Postgres pool + query latency.** Pool gauges read deadpool `Pool::status()`
  per project; the latency histogram wraps the awaited call at the four `db_span`
  sites (`db.operation` = query/execute/txn.query/txn.execute).
- **Per-component memory.** Bridges the D16 fork limiter
  (`WamnStoreLimiter`) — high-water, denial count, budget — through the read-only
  accessors the carried fork commit adds; the run-worker publishes its
  flowrunner store's state after each drive **only when a budget is configured**
  (`WAMN_MEMORY_LIMIT_MB`), mirroring the fork's own "attach only when budgeted"
  rule so the unbudgeted default is byte-identical. The high-water gauge reads
  the *allowed* size, never the budget.
- **Generated-API RPS.** The host owns the HTTP server; the `ProxyPre` bench
  path bypasses it, so the counter lives in the **fork**
  (`record_response_status`, the per-request choke point) and is verified only
  against the deployed api-gateway (in-cluster).

## Provider wiring

| Binary | Provider | Emits |
|---|---|---|
| `wamn-host` (`host`) | fork global (reused) | pool + query latency, memory (budgeted washlet stores), API RPS (fork) |
| `wamn-run-worker` | fork global (reused) | run executions, run-drive duration, pool + query latency, memory (its flowrunner store) |
| `wamn-dispatcher` | its own minimal provider (`main.rs`, no runtime linked) | run-queue depth |
| `wamn-gates` (`metricbench`) | fork global (reused) | drives all of the above |

**Env:** set `OTEL_METRIC_EXPORT_INTERVAL=1000` on the runner + dispatcher
manifests — the OTel periodic reader defaults to **60 s**, which would starve a
bounded gate; the reader honors the env var directly. `OTEL_EXPORTER_OTLP_ENDPOINT`
must also be present on the dispatcher (added in `deploy/platform/dispatcher.yaml`).

## Collector pipeline

`deploy/infra/otel-collector.yaml` (and `otelcol-local.yaml`) add a `prometheus`
exporter (`0.0.0.0:8889`, `add_metric_suffixes: false`) and a `metrics` pipeline
(`otlp` receiver → `batch` → `prometheus`), plus the `:8889` container/Service
port. This is distinct from the collector's OWN telemetry on `:8888`
(`otelcol_*`). The gate scrapes `http://otel-collector:8889/metrics` and greps
`wamn_*`.

## The gate — `metricbench`

`crates/wamn-gates/src/metricbench.rs` drives the production emission seams and
asserts each family in the `:8889` scrape (the metrics analog of `tracebench` →
Tempo / `logbench` → Loki):

1. N runs incl. one forced failure → `wamn_run_executions` +N with an
   `outcome="failed"` series;
2. seeded queue → `wamn_run_queue_depth` > 0 via a real dispatcher tick, drains
   to 0;
3. `wamn_run_drive_duration_ms_count` > 0;
4. a forced limiter denial → `wamn_memory_denied` > 0 and
   `wamn_memory_high_water_bytes` reads the allowed size, not the budget;
5. the drives' own DB writes surface `wamn_postgres_pool_size` +
   `wamn_postgres_query_duration_ms_count` > 0;
6. `wamn_api_requests` — **in-cluster only** (ProxyPre bypasses the host HTTP
   server), honest-skipped locally (traceproof-style).

Run recipe (local + in-cluster gate of record): `docs/build-and-test.md` §[9.8].

## Boundaries / deferred

- **Per-node duration p50/p99** — true per-node timing is guest-side (node_runs
  timestamps), like guest-minted `node_id` on the 9.1 spans. 9.8 ships the
  whole-run drive histogram; per-node is deferred (derivable by query over
  node_runs) — tracked as its own bead.
- **Host density (components per host)** — no washlet accessor exposes the
  bound-workload count; deferred (a fork accessor or a run-worker=1
  approximation) — tracked as its own bead.
- **Budgeted washlet-host stores** (api-gateway, custom nodes) — their limiter
  lives in fork-created stores the host does not hold, so their memory metrics
  await a process-wide meter sink in the fork; 9.8 wires the bridge + accessors
  and emits for the stores the host DOES drive (the run-worker flowrunner).

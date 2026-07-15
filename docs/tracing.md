# Tracing — the OTel trace pipeline (9.1)

Host-native spans → OTel Collector → **Tempo** (D13), enriched with execution
context, so a single trace threads a run from its trigger through the runner and
the `wamn:postgres` calls it makes. This is the traces third of Epic 9
(`docs/platform-plan.md` §9.1); logs (S5) already ship the same way to Loki.

- **Issue:** wamn-5gq `[9.1]`; **Epic:** wamn-7ok (Observability).
- **Blocks:** wamn-rvd `[9.2]` (traceparent propagation) + wamn-jn6 `[9.8]`
  (metrics).

## What is already free

The trace exporter is **not new**. wash-runtime's `initialize_observability`
(the fork's `observability.rs`, called by every `wamn-host`/`wamn-gates` entry
point) builds an OTLP `SpanExporter` → `TracerProvider` → a `tracing_opentelemetry`
layer on the process's global subscriber, **and** registers the W3C
`TraceContextPropagator` — all gated on `OTEL_*` env presence (the same
activation switch S5 logging uses). So any `tracing` span in the process is
bridged to an OTel span and exported over OTLP-gRPC whenever `OTEL_*` is set.

wash-runtime's HTTP path (`host/http.rs`) already emits:

- an `#[instrument]` inbound `handle_http_request` span (HTTP semconv method /
  path / **status class** — "HTTP status classes come free");
- an `invoke_component_handler` span carrying `workload.id/name/namespace`;
- outbound client spans; and it **extracts** an incoming `traceparent` so a
  request continues its caller's trace.

## What 9.1 adds (host-created spans, wamn-side)

9.1 keeps everything wamn-side — no fork patch. It adds the two host-owned spans
the plan names and wires the sink:

1. **`wamn.postgres` DB span** — the `wamn:postgres` plugin
   (`crates/wamn-host/src/plugins/wamn_postgres.rs`, `db_span`) wraps every
   guest DB call (one-shot `query`/`execute`, transaction `query`/`execute`) in
   a span carrying `db.system=postgresql`, `db.operation`, and — enriched
   host-side from the same claim maps that inject `app.tenant`, so the guest
   cannot spoof them — `wamn.tenant` / `wamn.project` / `wamn.component`. It's a
   plain `tracing::info_span!`, so it nests under whatever span is current (a
   request handler, or a trigger span) and threads into that trace.

2. **`wamn.trigger` span** — the dispatcher
   (`crates/wamn-host/src/dispatch.rs`, `trigger_span`) roots a fired run's
   trace with `wamn.flow` / `wamn.run_id` / `wamn.flow_version` /
   `wamn.trigger_source` / `wamn.tenant`. The dispatcher is the **host-known
   path**: it mints `flow`/`run_id` here, so this is their enrichment home. Both
   the cron and outbox (row-event) fire sites instrument their write-ahead under
   it.

3. **Tempo sink + collector traces pipeline** — `deploy/tempo.yaml`
   (single-binary Tempo, OTLP-gRPC in on :4317, TraceQL query API on :3200) and
   a `traces` pipeline in `deploy/otel-collector.yaml` (`otlp` receiver →
   `otlp/tempo` exporter). Local variants: `deploy/tempo-local.yaml` +
   `deploy/otelcol-local.yaml`.

### Enrichment sources

| Attribute | Source | Where |
| --- | --- | --- |
| `tenant`, `project` | host claim maps (component → tenant/project), non-spoofable | `db_span` (and `trigger_span` for tenant) |
| `flow`, `run_id` | minted by the dispatcher when it fires a run | `trigger_span` |
| HTTP method/path/status | wash-runtime (free) | `handle_http_request` |
| `node_id` | *(deferred — 9.2)* | — |

`tenant`/`project` are host claims (the S5 logging precedent). `flow`/`run_id`
live in the run's execution — the **host mints them on the dispatcher path**, so
that is where 9.1 enriches them. A webhook's `run_id` is minted *inside the
guest* and its per-node `node_id` lives inside the guest's node loop; surfacing
those to host spans needs the guest→host run-context contract, which is 9.2.

## Boundaries (9.2, not 9.1)

- **Cross-pod threading.** wash-runtime extracts an incoming `traceparent` but
  does **not inject** one on outbound `wasi:http` / `wamn:postgres` calls — so a
  trace threads within a process (via `tracing` parent/child → OTel) but breaks
  across pods (a custom-node hop, the generated-API pod). Host-enforced
  traceparent **stamping** on outbound calls is 9.2 (the `wamn:node`
  `traceparent` field is frozen at 5.4 but unwired).
- **Guest run context.** Guest-minted webhook `run_id` and per-node `node_id`
  enrichment ride the same 9.2 contract.

## The gate — `tracebench` (the S5 `logbench`→Loki analog)

`crates/wamn-gates/src/tracebench.rs` drives a real guest DB call (`pgprobe`
op 6, `SELECT pg_sleep(0)` — fixture-free) *under* the real `trigger_span`, so
the plugin's DB span nests beneath it. It then queries Tempo's TraceQL API
(`/api/search` by the intrinsic span name, `/api/traces/<id>` for the full
trace) and asserts **one trace** with:

- a `wamn.trigger` span carrying `wamn.flow` / `wamn.run_id` / `wamn.tenant`;
- a `wamn.postgres` DB span carrying `db.system` / `wamn.tenant` / `wamn.project`;
- the DB span threaded **under** the trigger span (`parentSpanId == trigger`).

That single trace is the `trigger → runner → wamn:postgres` thread of the plan's
acceptance script, proven through the **production span builders** (the gate
uses `wamn_host::dispatch::trigger_span` and the real plugin span), not gate
scaffolding.

### Run it

Local iteration (throwaway Postgres + Tempo + collector on a docker network):

```sh
docker network create wamn-s5 2>/dev/null || true
docker run -d --rm --name wamn-trace-pg --network wamn-s5 -p 5482:5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
docker run -d --name wamn-s5-tempo --network wamn-s5 -p 3200:3200 \
  -v "$PWD/deploy/tempo-local.yaml:/etc/tempo/tempo.yaml:ro" \
  grafana/tempo:2.6.1 -config.file=/etc/tempo/tempo.yaml
docker run -d --name wamn-s5-otelcol --network wamn-s5 -p 4317:4317 -p 8888:8888 \
  -v "$PWD/deploy/otelcol-local.yaml:/etc/otelcol/config.yaml:ro" \
  otel/opentelemetry-collector-contrib:0.115.1 --config=/etc/otelcol/config.yaml
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317 OTEL_EXPORTER_OTLP_PROTOCOL=grpc \
  OTEL_BSP_SCHEDULE_DELAY=1000 RUST_LOG=error \
  ./target/debug/wamn-gates --log-level info tracebench \
  --pgprobe components/target/wasm32-wasip2/release/pgprobe.wasm \
  --database-url postgres://postgres:postgres@127.0.0.1:5482/wamn \
  --tempo-url http://127.0.0.1:3200
```

`--log-level info` matters: the OTLP trace filter is level-tied and the spans are
`INFO`. In-cluster gate of record: `deploy/tracebench-job.yaml` (against real
Tempo + collector + Postgres; no CPU limit — the S2 lesson).

Mutation-tested (`scratchpad/mutate_9.1.py`): the DB span's tenant, the trigger
span's flow field, the DB span's name, and the DB span's parent inheritance
(threading) each flip a named `tracebench` assertion to FAIL.

## References

- Plan: `docs/platform-plan.md` §9.1; D13 (`Observability store`).
- S5 logging (the sibling signal + collector/Loki manifests):
  `docs/p0-results.md` S5.
- The fork observability init: `docs/wash-runtime-fork.md`.

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

3. **Tempo sink + collector traces pipeline** — `deploy/infra/tempo.yaml`
   (single-binary Tempo, OTLP-gRPC in on :4317, TraceQL query API on :3200) and
   a `traces` pipeline in `deploy/infra/otel-collector.yaml` (`otlp` receiver →
   `otlp/tempo` exporter). Local variants: `deploy/infra/tempo-local.yaml` +
   `deploy/infra/otelcol-local.yaml`.

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

## Cross-pod propagation (9.2)

9.1 threads a trace *within* a process; 9.2 (wamn-rvd) makes it thread *across*
one, **host-enforced**:

- **Host-enforced outbound inject.** wash-runtime already *extracts* an incoming
  `traceparent` (`handle_http_request`) but did not *inject* one on outbound
  calls. 9.2 adds the symmetric inject in `DefaultOutgoingHandler::send_request`
  (a carried fork commit — the single production outbound `wasi:http` send path;
  see `docs/wash-runtime-fork.md`): it stamps the current W3C trace context onto
  the outbound request before it leaves the process. Because **every** outbound
  `wasi:http` call flows through this handler regardless of whether the guest
  used an SDK, an SDK-bypassing custom node cannot break trace continuity — the
  guarantee is structural, not authorship-dependent.
- **SDK propagation helper.** `RunContext::trace_headers()` /
  `apply_trace_context()` (`wamn-node-sdk`) return the active `traceparent` /
  `tracestate` as header pairs, and the standard `http-request` node
  (`wamn-nodes`) forwards them onto the outbound request it builds (a config
  header of the same name still wins). The host inject makes this
  belt-and-braces for continuity; it keeps `traceparent` present on the node's
  own request and lets a node correlate on it.
- **`wamn:postgres`** needs no wire change (Postgres has no `traceparent`
  concept); the 9.1 DB span already nests under the current span, so the trace
  context is captured by parentage.

### The gate — `traceproof` (the deployed cross-pod proof)

The 9.1 in-proc `tracebench` cannot exercise this: `ProxyPre` bypasses
wash-runtime's HTTP server, so the outbound send path where the fork stamps the
context never runs. `traceproof` (`crates/wamn-gates/src/traceproof.rs`) runs
against **real deployed workloads**:

```text
  traceproof --GET, traceparent=00-T-S0-01--> trace-relay (wash pod A)
       relay makes a BARE outbound GET (no trace header) --------+
                                                                 v
                                     serve-echo (plain pod B) reflects the
                                     traceparent it received, as JSON
       <-- relay returns serve-echo's body verbatim ------------ +
```

`trace-relay` (`components/fixtures/trace-relay`) is wash-served and makes ONE
bare outbound call; it never sets a trace header, so a `traceparent` arriving at
`serve-echo` can only have been host-injected. The proof asserts (each a NAMED
failure a mutation of the inject flips): the downstream received a `traceparent`
at all; its **trace id equals the one we sent** (the trace threaded the
boundary); and its **span id differs** (the host minted a child client span, not
a blind copy). In-cluster gate of record: `deploy/gates/serve-echo.yaml` +
`deploy/platform/trace-relay-workload.yaml` + `deploy/gates/traceproof-job.yaml`.

### Deferred

- **Guest-visible run context on host-invoked flows.** The `flowrunner` is
  invoked via an exported `run()` (no inbound HTTP), so it has no request header
  to source `RunContext.traceparent` from; its outbound calls are still traced
  (host inject), but surfacing a per-run `traceparent` to it needs the
  queue/dispatch path to carry one (follow-up).
- **Guest-minted enrichment.** Webhook-minted `run_id` and per-node `node_id` on
  host spans still await the guest→host run-context contract.

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
uses `wamn_dispatcher::trigger_span` and the real plugin span), not gate
scaffolding.

### Run it

Local iteration (throwaway Postgres + Tempo + collector on a docker network):

```sh
docker network create wamn-s5 2>/dev/null || true
docker run -d --rm --name wamn-trace-pg --network wamn-s5 -p 5482:5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
docker run -d --name wamn-s5-tempo --network wamn-s5 -p 3200:3200 \
  -v "$PWD/deploy/infra/tempo-local.yaml:/etc/tempo/tempo.yaml:ro" \
  grafana/tempo:2.6.1 -config.file=/etc/tempo/tempo.yaml
docker run -d --name wamn-s5-otelcol --network wamn-s5 -p 4317:4317 -p 8888:8888 \
  -v "$PWD/deploy/infra/otelcol-local.yaml:/etc/otelcol/config.yaml:ro" \
  otel/opentelemetry-collector-contrib:0.115.1 --config=/etc/otelcol/config.yaml
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317 OTEL_EXPORTER_OTLP_PROTOCOL=grpc \
  OTEL_BSP_SCHEDULE_DELAY=1000 RUST_LOG=error \
  ./target/debug/wamn-gates --log-level info tracebench \
  --pgprobe components/target/wasm32-wasip2/release/pgprobe.wasm \
  --database-url postgres://postgres:postgres@127.0.0.1:5482/wamn \
  --tempo-url http://127.0.0.1:3200
```

`--log-level info` matters: the OTLP trace filter is level-tied and the spans are
`INFO`. In-cluster gate of record: `deploy/gates/tracebench-job.yaml` (against real
Tempo + collector + Postgres; no CPU limit — the S2 lesson).

Mutation-tested (`scratchpad/mutate_9.1.py`): the DB span's tenant, the trigger
span's flow field, the DB span's name, and the DB span's parent inheritance
(threading) each flip a named `tracebench` assertion to FAIL.

## References

- Plan: `docs/platform-plan.md` §9.1; D13 (`Observability store`).
- S5 logging (the sibling signal + collector/Loki manifests):
  `docs/p0-results.md` S5.
- The fork observability init: `docs/wash-runtime-fork.md`.

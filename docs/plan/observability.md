# Observability plan

@D Kloimwieder

Status: direction, 2026-10-15. Evidence cites the repo's `main` by file and symbol; fact unless marked inference. Rulings R1–R8 are the owner's; §7 is the work.

## How this plan is worked

The top list of `platform-deploy.md` ("How this plan is worked") applies unchanged. Code commits never edit this plan; a plan edit is a docs commit that cites its ruling.

## 1. Goal

Every trace, metric and log line the platform emits lands in one bucket as columnar files, queryable with SQL, at object-storage cost, with nothing else to run. A dashboard, when one is wanted, reads the same bucket; the hosts never change for it.

```
host ──OTLP──▶ collector ──Parquet──▶ gs://wamn-<project>-otel/{traces,metrics,logs}/… ──▶ DuckDB (now) / Grafana (later)
```

## 2. What exists

- The host instruments every effect with `tracing` spans through `effect_span!` (`crates/platform/runtime/src/plugins/effect_span.rs`): `wamn.component.invoke`, `wamn.router.resolve`, `wamn.auth.identity`, `wamn.auth.permissions`, `wamn.postgres.{begin,execute,query,commit,decode_rows,session_settings}`, `wamn.queue.delivery`, `wamn.connection_http`, plus `record_query_ms` timings. Span fields carry org, project, operation, component and run id.
- The host calls `wash_runtime::observability::initialize_observability` (`services/host/src/main.rs:67`); OTLP trace, metric and log exporters activate when `OTEL_EXPORTER_OTLP_ENDPOINT` is set and are no-ops otherwise. `wasi-otel` is on (`services/host/Cargo.toml:19`), so guest spans join the host trace.
- The edge links no exporter by lint (`tools/repo-lint-dependencies`). Unchanged.
- On wamn-dev nothing is set: no `OTEL_*` in `deploy/gcp/host-group-*.yaml` or the release chart; the cluster runs `--logging=SYSTEM --monitoring=SYSTEM` (gcp.md §2.4). Traces drop. Observability today is `kubectl logs` and the run tables.

## 3. Rulings

| | Ruling | Why |
| --- | --- | --- |
| R1 | **The backend is object storage. No per-event-priced service receives platform telemetry.** | Cloud Trace at 500 req/s unsampled is ~$2,000/month; the same volume as gzip Parquet in GCS is ~$20 (§6). |
| R2 | **One OpenTelemetry Collector per cluster, in `platform`, is the only thing the hosts talk to.** | The hosts' config is one endpoint; every backend change is a collector config change. The collector is vendor-neutral. No Fluent Bit: it cannot receive OTLP spans and would convert twice. |
| R3 | **The collector writes Parquet, partitioned `signal/year=/month=/day=/hour=`, one object per flush.** | Parquet is ~10:1 over JSON for spans and is what DuckDB, Polars and every warehouse read. Hour partitions keep a query to the files it needs. |
| R4 | **Batch 60 s or 50,000 items, whichever first; a persistent sending queue; a memory limiter.** | 60 objects/hour/signal at any rate (§5); a GCS blip loses nothing; a burst cannot OOM the pod. |
| R5 | The host samples 10% of new traces at their start and keeps sampled parents. The collector does not sample traces. | Owner ruling 2026-10-18 (wamn-spv0f). At 100% sampling, tracing and OTel export used 13.8% of host CPU (wamn-kb8c9). One host configuration owns the rate. |
| R6 | **The host's OTLP endpoint is chart-owned: the `release` chart sets `OTEL_EXPORTER_OTLP_ENDPOINT` and `OTEL_SERVICE_NAME` on every host it renders; the control host group file sets the same two.** | The variables are not an application fact; the document does not name them (R21 of platform-deploy). `REFUSED_VARIABLES` in `release_chart.rs:260` gains both. |
| R7 | **The bucket has a 30-day lifecycle. Nothing deletes objects by hand.** | Retention is a bucket rule, not a verb (standing rule: no deletion of Cloud Storage objects). |
| R8 | **Dashboards read the bucket. Nothing is ingested twice.** | §8. |

## 4. Collector

One Deployment `otel-collector` in namespace `platform`, image `otel/opentelemetry-collector-contrib` pinned by digest, 1 replica, Spot, requests 100m/256Mi, limits 500m/1Gi.

```yaml
receivers:
  otlp: { protocols: { grpc: { endpoint: 0.0.0.0:4317 } } }
processors:
  memory_limiter: { check_interval: 1s, limit_mib: 800, spike_limit_mib: 200 }
  resource: { attributes: [{ key: wamn.cluster, value: "${CLUSTER}", action: upsert }] }
  batch: { timeout: 60s, send_batch_size: 50000, send_batch_max_size: 100000 }
exporters:
  googlecloudstorage/traces:  { bucket: "${BUCKET}", prefix: traces,  encoding: parquet, partition: hour, compression: gzip, sending_queue: { storage: file_storage } }
  googlecloudstorage/metrics: { bucket: "${BUCKET}", prefix: metrics, encoding: parquet, partition: hour, compression: gzip, sending_queue: { storage: file_storage } }
  googlecloudstorage/logs:    { bucket: "${BUCKET}", prefix: logs,    encoding: parquet, partition: hour, compression: gzip, sending_queue: { storage: file_storage } }
extensions:
  file_storage: { directory: /var/lib/otelcol/queue }
service:
  extensions: [file_storage]
  pipelines:
    traces:  { receivers: [otlp], processors: [memory_limiter, resource, batch], exporters: [googlecloudstorage/traces] }
    metrics: { receivers: [otlp], processors: [memory_limiter, resource, batch], exporters: [googlecloudstorage/metrics] }
    logs:    { receivers: [otlp], processors: [memory_limiter, resource, batch], exporters: [googlecloudstorage/logs] }
```

Inference, to confirm in issue 1: the contrib image's GCS exporter writes OTLP-JSON today; Parquet encoding may need the `file` exporter with Parquet encoding plus a GCS sync sidecar, or the `awss3` exporter against the GCS S3-compatible endpoint, which does Parquet. Whichever builds from the pinned contrib image without a custom build wins; a custom collector build is refused. If only JSON is available, ship gzip NDJSON and note that DuckDB `read_json_auto` reads it; convert to Parquet later with one DuckDB `COPY`.

Identity: KSA `otel-collector` in `platform`, Workload Identity to a GSA `wamn-otel` with `roles/storage.objectCreator` on the bucket only. Owner input, like `wamn-blob`.

Local dev and kind: the same Deployment with the `file` exporter to an emptyDir; `wamn dev up` already takes `--otel-exporter-otlp-endpoint` (`services/ctl/src/dev/up.rs:68`).

## 5. Objects and cost

Objects per hour per signal = flushes per hour = 60 at a 60 s batch, at any request rate (R4). At 500 req/s × 8 spans, one object is ~2.4M spans ≈ 30 MiB gzip Parquet, which is a good DuckDB file size. At dev rate an object is kilobytes; small files are fine for DuckDB and cost $0.005 per 1,000 writes.

| | Spans/month | Bucket resident (30 d) | Storage | Writes | Collector | Month |
| --- | --- | --- | --- | --- | --- | --- |
| wamn-dev, pool mostly at 0 | <50M | <5 GiB | $0.10 | $0.30 | shares the pool | **~$1** |
| 50 req/s sustained | 1B | ~110 GiB (spans 100, logs 10, metrics 0.5) | $2.20 | $0.65 | shares the pool | **~$3** |
| 500 req/s sustained | 10B | ~1 TiB | $20 | $1 | own e2-standard-2 Spot, $15 | **~$36** |
| 500 req/s, 10% sampled | 1B | ~110 GiB | $2.20 | $0.65 | shares the pool | **~$3** |

Assumptions: us-central1 Standard storage $0.02/GiB-month, Class A $0.005/1k, ~100 bytes per span and ~200 per log line in gzip Parquet (inference from typical OTLP Parquet; measured in issue 3). Reads: Class B $0.0004/1k; egress is free to a VM in the region and $0.12/GiB to a laptop, so ad-hoc DuckDB runs on the bench VM or a Cloud Shell, not over the wire.

## 6. Querying now

DuckDB with `httpfs` and a service-account key with `storage.objectViewer`:

```sql
INSTALL httpfs; LOAD httpfs;
SET s3_endpoint='storage.googleapis.com'; SET s3_access_key_id='…'; SET s3_secret_access_key='…';
-- p99 by span name for one benchmark hour
SELECT name, count(*) AS n, quantile_cont(duration_ns, 0.5)/1e6 AS p50_ms, quantile_cont(duration_ns, 0.99)/1e6 AS p99_ms
FROM read_parquet('s3://wamn-dev-otel/traces/year=2026/month=10/day=15/hour=18/*.parquet')
GROUP BY name ORDER BY n DESC;
-- where the time goes under one entry: child spans joined on trace_id
SELECT c.name, sum(c.duration_ns)/sum(p.duration_ns) AS share
FROM read_parquet('s3://…/traces/…/*.parquet') p
JOIN read_parquet('s3://…/traces/…/*.parquet') c USING (trace_id)
WHERE p.name = 'wamn.component.invoke' AND c.parent_span_id = p.span_id
GROUP BY c.name ORDER BY share DESC;
```

`docs/operations/observability.md` holds these two queries and the bucket layout; nothing else.

## 7. Work

Epic "observability", three issues, in order. No kind run; the acceptance is the benchmark.

1. **Collector and bucket.** `deploy/platform/otel-collector.yaml` (Deployment, ConfigMap, KSA, PVC 1Gi for the queue) and `deploy/gcp/otel-collector-values.yaml` (bucket, cluster name). Owner inputs added to gcp.md §1: bucket `wamn-dev-otel` with the 30-day lifecycle, GSA `wamn-otel`, the Workload Identity binding. gcp.md §4 gains one step after 4.4: apply the collector. Settle the Parquet-vs-JSON inference and record it on the bead. Test: `helm template`/`kubectl apply --dry-run` of the manifest; a unit test that the config parses with the pinned image's `validate` subcommand.
2. **Host endpoint.** `release_chart::values` sets `OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector.platform.svc.cluster.local:4317` and `OTEL_SERVICE_NAME=wamn-host`; both join `REFUSED_VARIABLES`. `deploy/gcp/host-group-*.yaml` and the control host group set the same. The kind cutover values point at the local collector. Platform patch bump (rule 6). Test: the existing chart render test asserts both variables once.
3. **Acceptance = the benchmark rerun** (bead already filed): tier 2 on 0.1.x with the collector in place. Pass: the benchmark hour is in the bucket, the §6 queries answer, and `docs/operations/benchmark.md` shows the p99 breakdown by span name beside the client-side numbers. Record measured bytes-per-span to replace the §5 inference.

Not in this epic: dashboards (§8), sampling (R5), alerting, workload logging on the cluster (the collector receives host logs over OTLP; `--logging=WORKLOAD` stays off).

## 8. Dashboards, later

The bucket is the only store. A dashboard is a reader of it, added when someone needs to look without writing SQL. Three readers fit R8, in the order they would be tried:

1. **Grafana + DuckDB datasource** (the `grafana-duckdb-datasource` plugin, or Grafana's Infinity datasource over a tiny HTTP shim running DuckDB). One Grafana pod, no ingest, panels are the §6 queries. Cheapest; latency is a DuckDB scan per panel, seconds over a day's files. Fine for a dev platform and for "what happened at 14:00".
2. **Grafana Tempo and Mimir in object-storage mode, reading the same bucket's raw OTLP** when a tenant needs trace-by-id lookup and sub-second metric panels. Tempo ingests from the collector (a second exporter in the fan-out, not a second host config) and stores blocks in its own prefix of the same bucket; the Parquet prefix stays for SQL. This is the one case where data lands twice, and it is the collector's fan-out, not the hosts', so R8 holds at the host.
3. **BigQuery external tables over the Parquet prefix** for the finance view: no ingest, per-query pricing, Looker Studio on top. Only if someone outside engineering wants a chart.

What stays fixed across all three: the host variables (R6), the collector as the one receiver (R2), the bucket layout (R3), and the §6 queries, which become the panels. What is decided then, not now: which reader, retention beyond 30 days (a second bucket class, not a change here), and whether metrics move to a 60 s interval.

## 9. Open questions

1. Parquet from the pinned contrib image without a custom build (issue 1 settles it).
2. Does `wasi-otel` emit guest spans for the Receiving component today, or only host spans? The first benchmark hour answers it; if only host spans, that is a finding on the host, not this plan.
3. Metric interval: wash-runtime's default export interval, to read from the first hour's metric object count.

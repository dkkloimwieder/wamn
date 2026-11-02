# Observability plan

@D Kloimwieder

## How this plan is worked

The top list of `platform-deploy.md` ("How this plan is worked") applies unchanged. Code commits never edit this plan; a plan edit is a docs commit that cites its ruling.

## 1. Goal

Every trace, metric and log line the platform emits lands in one bucket as columnar files, queryable with SQL, at object-storage cost, with nothing else to run. A dashboard, when one is wanted, reads the same bucket; the hosts never change for it.

## 2. What exists

- The edge links no exporter by lint (`tools/repo-lint-dependencies`). Unchanged.

## 3. Rulings

| | Ruling | Why |
| --- | --- | --- |
| R1 | **The backend is object storage. No per-event-priced service receives platform telemetry.** | |
| R2 | **One OpenTelemetry Collector per cluster, in `platform`, is the only thing the hosts talk to.** | The hosts' config is one endpoint; every backend change is a collector config change. The collector is vendor-neutral. No Fluent Bit: it cannot receive OTLP spans and would convert twice. |
| R3 | **The collector writes Parquet, partitioned `signal/year=/month=/day=/hour=`, one object per flush.** | |
| R4 | **Batch 60 s or 50,000 items, whichever first; a persistent sending queue; a memory limiter.** | a GCS blip loses nothing; a burst cannot OOM the pod. |
| R5 | The host samples 10% of new traces at their start and keeps sampled parents. The collector does not sample traces. | Owner ruling 2026-10-18 (wamn-spv0f). One host configuration owns the rate. |
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

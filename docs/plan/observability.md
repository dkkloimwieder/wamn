# Observability plan

@D Kloimwieder

## How this plan is worked

See [standing repository rules](../../CLAUDE.md#standing-repository-rules), [platform version procedure](../operations/building.md#platform-version-procedure), and [cloud prerequisites](../operations/gcp.md#cloud-credentials-and-prerequisites). Code commits never edit this plan; a plan edit is a docs commit that cites its ruling.

## 1. Goal

See [telemetry storage and queries](../operations/observability.md).

## 2. What exists

See [edge capability boundaries](../architecture/capabilities.md).

## 3. Rulings

| | Ruling | Why |
| --- | --- | --- |
| R1 | See [telemetry storage and queries](../operations/observability.md). | |
| R2 | See [telemetry storage and queries](../operations/observability.md). | |
| R3 | **The collector writes Parquet, partitioned `signal/year=/month=/day=/hour=`, one object per flush.** | |
| R4 | See [telemetry storage and queries](../operations/observability.md). | |
| R5 | See [telemetry storage and queries](../operations/observability.md). | |
| R6 | See [telemetry storage and queries](../operations/observability.md). | |
| R7 | See [telemetry storage and queries](../operations/observability.md). | |
| R8 | **Dashboards read the bucket. Nothing is ingested twice.** | §8. |

## 4. Collector

See [the current collector](../operations/observability.md).

Inference, to confirm in issue 1: the contrib image's GCS exporter writes OTLP-JSON today; Parquet encoding may need the `file` exporter with Parquet encoding plus a GCS sync sidecar, or the `awss3` exporter against the GCS S3-compatible endpoint, which does Parquet. Whichever builds from the pinned contrib image without a custom build wins; a custom collector build is refused. If only JSON is available, ship gzip NDJSON and note that DuckDB `read_json_auto` reads it; convert to Parquet later with one DuckDB `COPY`.

Not in this epic: dashboards (§8), sampling (R5), alerting, workload logging on the cluster (the collector receives host logs over OTLP; `--logging=WORKLOAD` stays off).

## 8. Dashboards, later

The bucket is the only store. A dashboard is a reader of it, added when someone needs to look without writing SQL. Three readers fit R8, in the order they would be tried:

1. **Grafana + DuckDB datasource** (the `grafana-duckdb-datasource` plugin, or Grafana's Infinity datasource over a tiny HTTP shim running DuckDB). One Grafana pod, no ingest, panels are the §6 queries. Cheapest; latency is a DuckDB scan per panel, seconds over a day's files. Fine for a dev platform and for "what happened at 14:00".
2. **Grafana Tempo and Mimir in object-storage mode, reading the same bucket's raw OTLP** when a project needs trace-by-id lookup and sub-second metric panels. Tempo ingests from the collector (a second exporter in the fan-out, not a second host config) and stores blocks in its own prefix of the same bucket; the Parquet prefix stays for SQL. This is the one case where data lands twice, and it is the collector's fan-out, not the hosts', so R8 holds at the host.
3. **BigQuery external tables over the Parquet prefix** for the finance view: no ingest, per-query pricing, Looker Studio on top. Only if someone outside engineering wants a chart.

What stays fixed across all three: the host variables (R6), the collector as the one receiver (R2), the bucket layout (R3), and the §6 queries, which become the panels. What is decided then, not now: which reader, retention beyond 30 days (a second bucket class, not a change here), and whether metrics move to a 60 s interval.

## 9. Open questions

1. Parquet from the pinned contrib image without a custom build (issue 1 settles it).
2. Does `wasi-otel` emit guest spans for the Receiving component today, or only host spans? The first benchmark hour answers it; if only host spans, that is a finding on the host, not this plan.
3. Metric interval: wash-runtime's default export interval, to read from the first hour's metric object count.

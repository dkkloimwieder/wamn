# Observability

The collector `platform/otel-collector` receives the spans and logs of every host over OTLP. It writes them to the bucket `gs://wamn-dev-otel` ([gcp.md](gcp.md) section 4.4.1). The objects are gzip OTLP JSON, one object per flush:

```text
gs://wamn-dev-otel/traces/year=YYYY/month=MM/day=DD/hour=HH/traces_<uuid>.gz
gs://wamn-dev-otel/logs/year=YYYY/month=MM/day=DD/hour=HH/logs_<uuid>.gz
```

The hour in the path is UTC. The collector writes no metrics.

The existing head sampler controls WAMN request span creation as well as trace export.
Unsampled requests create no WAMN child spans, but retain the two upstream HTTP spans.
See [execution observability](../architecture/execution.md) for context propagation and behavior without an active trace.

## Query one hour

Reading the objects needs `storage.objectViewer` on the bucket. Copy the hour to a local directory, and load its spans into DuckDB:

```bash
mkdir -p otel/h01
gcloud storage cp "gs://wamn-dev-otel/traces/year=2026/month=10/day=16/hour=01/*" otel/h01/
cd otel && duckdb spans.db
```

```sql
CREATE OR REPLACE TABLE spans AS
WITH f AS (SELECT resourceSpans FROM read_json('h01/*.gz', format='newline_delimited',
             maximum_object_size=1000000000, columns={resourceSpans:'JSON[]'})),
r AS (SELECT unnest(resourceSpans) rs FROM f),
s AS (SELECT unnest(CAST(rs->'scopeSpans' AS JSON[])) ss FROM r),
p AS (SELECT unnest(CAST(ss->'spans' AS JSON[])) sp FROM s)
SELECT sp->>'traceId' AS trace_id, sp->>'spanId' AS span_id, sp->>'parentSpanId' AS parent_span_id,
  sp->>'name' AS "name", CAST(sp->>'startTimeUnixNano' AS HUGEINT) AS start_ns,
  CAST(sp->>'endTimeUnixNano' AS HUGEINT) - CAST(sp->>'startTimeUnixNano' AS HUGEINT) AS duration_ns
FROM p;
```

The p50 and p99 by span name:

```sql
SELECT "name", count(*) AS n, quantile_cont(duration_ns, 0.5)/1e6 AS p50_ms,
  quantile_cont(duration_ns, 0.99)/1e6 AS p99_ms
FROM spans GROUP BY "name" ORDER BY n DESC;
```

The time share of each child span under one parent:

```sql
SELECT c."name", sum(c.duration_ns)/sum(p.duration_ns) AS share
FROM spans p JOIN spans c ON c.trace_id = p.trace_id AND c.parent_span_id = p.span_id
WHERE p."name" = 'wamn.component.invoke'
GROUP BY c."name" ORDER BY share DESC;
```

To limit the queries to one run, add a condition on `start_ns`.

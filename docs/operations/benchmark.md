# Benchmark

This page holds the tier 2 benchmark of Receiving on `wamn-dev` with the collector in place (wamn-ini3j). The procedure is in [the install history](../history/gcp-install-history.md#64-bench-client-and-pat), sections 6.4 and 6.5. The raw files are in `tests/bench/`. Latency is in milliseconds.

## Setup

The runs of 2026-10-16 used Receiving release `sha256:b3813ee613efb0caae4f15685b86438e7108587bcfbf46fc3034b0cdc2dd1cba`. The platform was 0.1.18 (`bd08a13b4`), and the bench was commit `7189b3020`. The pool `main` had 2 x `e2-standard-2` Spot nodes, which is tier 2. The client was the VM `wamn-bench-client`, an `e2-small` in `us-central1-a`. The database held the 1000 seed: 1000 purchase orders and 500500 lines. Each run lasted 60 seconds and had 0 errors.

Every host exported each span to the collector, with no sampling. The collector wrote the spans to `gs://wamn-dev-otel` ([observability](observability.md)).

The project database has no `pg_stat_statements`, so each Postgres sample in the result files holds `pg_stat_database` only. No `wamn-dev` environment serves Acme, so `acme_record_receipt` did not run.

## Client numbers

The baseline ran on 2026-09-28 on platform 0.1.0, with no exporter ([deployment plan](../history/gcp-deployment.md#61-tier-results)). The 0.1.18 runs ran from 01:31 to 01:41 UTC.

| Call | Concurrency | Baseline throughput | 0.1.18 throughput | Baseline p50 / p95 / p99 | 0.1.18 p50 / p95 / p99 |
| --- | --- | --- | --- | --- | --- |
| get | 4 | 256/s | 224/s | 11.3 / 26.6 / 55.3 | 15.3 / 23.1 / 37.1 |
| get | 16 | 626/s | 385/s | 22.7 / 37.5 / 55.5 | 38.2 / 56.8 / 80.2 |
| update | 4 | 208/s | 155/s | 15.8 / 27.9 / 50.3 | 23.1 / 30.9 / 39.0 |
| update | 16 | 402/s | 255/s | 35.3 / 61.1 / 109.0 | 55.0 / 87.1 / 128.6 |
| query | 4 | 152/s | 120/s | 22.3 / 32.6 / 53.8 | 28.7 / 42.0 / 62.3 |
| query | 16 | 235/s | 168/s | 64.5 / 89.5 / 140.4 | 89.6 / 127.2 / 181.4 |

Throughput is 13 to 38 percent below the baseline.

## Control without the collector

The control ran get and update at concurrency 16 on the same pool, from 02:10 to 02:26 UTC. The run without the collector removed `OTEL_EXPORTER_OTLP_ENDPOINT` and `OTEL_SERVICE_NAME` from the Receiving host Deployment. If any `OTEL_*` variable is present, the host turns on its exporter (`crates/platform/runtime/src/plugins/wamn_logging.rs`). Thus both variables must go. Each change restarts the host pod, so the second run with the collector ran on a new pod, after the rollback.

| Call c16 | Run | Throughput | p50 / p95 / p99 |
| --- | --- | --- | --- |
| get | baseline, 2026-09-28 | 626/s | 22.7 / 37.5 / 55.5 |
| get | with collector, 01:33 | 385/s | 38.2 / 56.8 / 80.2 |
| get | with collector, 02:10 | 414/s | 33.0 / 51.8 / 95.9 |
| get | without collector, new pod, 02:19 | 299/s | 49.2 / 71.5 / 109.3 |
| get | with collector, new pod, 02:24 | 406/s | 35.5 / 52.8 / 76.6 |
| update | baseline, 2026-09-28 | 402/s | 35.3 / 61.1 / 109.0 |
| update | with collector, 01:41 | 255/s | 55.0 / 87.1 / 128.6 |
| update | with collector, 02:11 | 231/s | 55.3 / 97.8 / 327.3 |
| update | without collector, new pod, 02:20 | 145/s | 97.8 / 171.8 / 282.0 |
| update | with collector, new pod, 02:25 | 268/s | 50.7 / 85.7 / 167.6 |

The collector does not account for the gap. The host without any `OTEL_*` variable is slower than the host with the collector. A first control removed only the endpoint. It measured a host that exported to the default `localhost:4317` and failed, at 288/s for get and 159/s for update. The files of the valid control replaced its files.

## Span breakdown

These are the spans of the 0.1.18 runs at concurrency 16, from the queries of [observability](observability.md). Each request makes about 22 spans. `handle_http_request` is the request in the host, and `invoke_component_handler` is its one child.

| Span | get p50 | get p99 | update p50 | update p99 |
| --- | --- | --- | --- | --- |
| `handle_http_request` | 29.7 | 55.8 | 45.9 | 95.9 |
| `wamn.route.authenticate` | 10.6 | 24.1 | 5.2 | 20.2 |
| `wamn.auth.permissions` | 9.3 | 21.9 | 3.1 | 17.1 |
| `wamn.auth.perm.query` | 0.9 | 4.0 | 1.3 | 6.0 |
| `wamn.auth.identity` | 1.0 | 5.2 | 1.6 | 7.4 |
| `wamn.component.invoke` | 4.2 | 11.5 | 27.9 | 69.1 |
| `wamn.postgres` | 2.1 | 7.4 | 6.2 | 23.7 |
| `wamn.claims.principal.query` | 0.9 | 4.5 | 1.6 | 6.7 |
| `wamn.postgres.bind_claims` | 0.8 | 4.7 | 1.6 | 6.6 |
| `wamn.postgres.acquire` | 0.02 | 17.6 | 0.02 | 10.8 |

At get c16, `wamn.auth.permissions` spends 87 percent of its 9.3 ms mean in `wamn.postgres.acquire`, which waits for a pooled connection. `invoke_component_handler` waits another 4.0 ms mean in `wamn.postgres.acquire`. At update c16, `wamn.component.invoke` takes 29.7 ms mean, and its Postgres children take about 11 ms of that. The client p50 of get c16 is 8.5 ms above the host p50, which is the time in the load balancer, the edge and the network.

## Bytes per span

The benchmark hour holds 4321511 spans in 110 objects of 273851478 bytes. One span is 63 bytes in gzip OTLP JSON and 1036 bytes before compression. The plan inferred 100 bytes per span and 8 spans per request. The measured request is 22 spans, about 1.4 KB compressed.

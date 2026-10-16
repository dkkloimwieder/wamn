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

## Pool size 8 and node placement

Commit `6458911fe` sets the guest pool to 16 and the platform pool to 8 in both wamn-dev host groups. The pool `main` has 3 nodes of 2 vCPU. The scheduler places the Receiving host and `wamn-pg-1` freely, because the runtime-operator 2.10.3 subchart has no affinity setting. To separate them, cordon the Postgres node, delete the host pod, and uncordon the node after the rollout.

| Call c16 | Run | Throughput | p50 / p95 / p99 |
| --- | --- | --- | --- |
| get | 0.1.18, pool 8, host on the Postgres node, 02:49 | 291/s | 49.9 / 79.2 / 115.2 |
| get | 0.1.19, pool 8, host on its own node, 10:14 | 531/s | 28.1 / 42.5 / 60.6 |
| update | 0.1.18, pool 8, host on the Postgres node, 02:50 | 167/s | 83.8 / 141.2 / 213.4 |
| update | 0.1.19, pool 8, host on its own node, 10:15 | 240/s | 54.7 / 109.2 / 229.1 |
| update | 0.1.19, pool 8, host on its own node, 10:21 | 266/s | 54.4 / 87.7 / 151.9 |

All runs had 0 errors. Pool size 8 removed the wait for a connection. The p50 of `wamn.postgres.acquire` under `wamn.auth.permissions` fell from 8 ms to 0.01 ms. At 02:49, the host and `wamn-pg-1` shared one node, and each SQL span was about 4 times slower than at 02:24. For example, `wamn.auth.perm.query` went from 0.9 ms to 3.5 ms. Keep the host off the Postgres node for every benchmark run.

At 10:15, the collector failed to upload part of its data with `failed to upload traces: context deadline exceeded`. The traces of that hour hold 11939 of the 31925 get requests and no update request. The run at 10:21 repeated update, and its traces hold 15815 of 16009 requests.

## Named parts

These are the host times of the 0.1.19 runs at 10:14 (get) and 10:21 (update), in ms. Each part is the time of one span without its child spans, or the time of a leaf span. The means of the parts add up to the mean of `invoke_component_handler`, which is the request in the host. The p50 values do not add up. `handle_http_request` ends when the response starts, before `invoke_component_handler` ends, so it is not the total.

| Part | Source | get p50 | get mean | update p50 | update mean |
| --- | --- | --- | --- | --- | --- |
| wash-runtime invoke | `invoke_component_handler` without its children | 6.15 | 6.92 | 5.50 | 6.09 |
| record_actor_labels query | `wamn.route.deliver` without its children | 4.01 | 4.61 | 3.87 | 4.49 |
| identity | `wamn.auth.identity` | 2.20 | 2.58 | 1.97 | 2.52 |
| permission query | `wamn.auth.perm.query` | 1.82 | 2.11 | 1.66 | 2.14 |
| principal claims query | `wamn.claims.principal.query` | 1.47 | 1.92 | 1.91 | 2.39 |
| bind claims | `wamn.postgres.bind_claims` | | | 1.74 | 2.18 |
| session settings | `wamn.postgres.session_settings` | 1.75 | 2.04 | | |
| application statement | `wamn.postgres.statement` and `decode_rows` | 1.73 | 2.01 | 6.79 | 8.44 |
| write-log claim | `wamn.write_log.claim` | | | 3.20 | 3.68 |
| item commit | `wamn.operation.commit` | | | 8.42 | 12.11 |
| write-log finish | `wamn.write_log.finish` | | | 4.60 | 5.60 |
| operation instantiation | `wamn.operation.instantiate` | 0.19 | 0.21 | 0.19 | 0.22 |
| guest execution | `wamn.operation.call` without its children | 0.30 | 0.38 | 0.35 | 0.38 |
| dispatch, request hash and answer | `wamn.component.invoke` without its children | 0.35 | 0.39 | 0.55 | 0.61 |
| route match, input validation, permit | `wamn.route.match`, `validate_input`, `permit` | 0.06 | 0.08 | 0.09 | 0.10 |
| router taps | `wamn.jetstream`, two per request | 0.02 | 0.09 | 0.02 | 0.06 |
| pool acquire | every `wamn.postgres.acquire` | 0.02 | 0.25 | 0.02 | 0.11 |
| span bookkeeping | `wamn.route.authenticate`, `wamn.auth.permissions`, `wamn.postgres`, `wamn.operation.begin_item` without their children | 0.02 | 0.16 | 0.02 | 0.19 |
| total | `invoke_component_handler` | 22.19 | 23.76 | 47.13 | 51.33 |

The wash-runtime invoke part is the store creation, the instantiation and the guest execution of the `http_route` component, which wamn does not split further. The record_actor_labels query is the Postgres call that `RouterDeliveryBridge::deliver` makes after the second router tap (`crates/execution/host/src/router_delivery.rs`). The trace shows it as one `wamn.postgres.acquire` child and 3.8 ms after it, until `wamn.route.deliver` ends. The query has no span of its own.

For get, wash-runtime invoke is the largest part. For update, the item commit and the application statement are larger. Postgres work is 13.3 ms of the get mean and 37.4 ms of the update mean.

## Postgres floor

pgbench ran in a pod in the cluster against `wamn-pg-1`, at scale 10 with 16 clients for 60 s.

| Script | TPS | p50 | p99 |
| --- | --- | --- | --- |
| select-only | 10377 | 0.34 | 6.36 |
| simple-update | 1888 | 6.88 | 23.04 |

The item commit p50 of update (8.42 ms) is near the simple-update p50 of pgbench (6.88 ms), which also ends in a commit.

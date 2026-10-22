# Benchmark

This page holds the tier 2 benchmark of Receiving on `wamn-dev` with the collector in place (wamn-ini3j).
The current procedure and regression gate are in [Google Cloud operations](gcp.md#58-benchmark).
`tools/bench/tier.sh` writes one result per platform version to `tests/bench/platform-<version>.json`.
The [0.1.25 result](../../tests/bench/platform-0.1.25.json) supplies the first baseline.
Git retains earlier per-run files.
Latency is in milliseconds.

## Local run

Run `tools/bench/local.sh` from this checkout on an eight-core Linux machine.
It builds release binaries and guests because debug builds do not represent host timing.
It builds the host in a separate Cargo call with the Dockerfile's packages and binaries.

It requires Docker, Helm, Python 3, `psql`, `curl`, `taskset`, and the build prerequisites in [building](building.md).
Prepare SQLx and web dependencies as described in [running tests](running-tests.md#application-generation-and-sqlx).
The Docker images `postgres:18`, `nats:2.10-alpine`, the collector image pinned in `deploy/platform/otel-collector.yaml`, and `nginx:1.27-alpine` must already exist locally.
The script does not pull images.

The script starts its own PostgreSQL, scheduler NATS, event NATS, and collector containers on free loopback ports.
It runs `wamn build` for Receiving before it renders the broker permissions.

At run time, it reads the event broker configuration and memory limit from `deploy/gcp/nats-jetstream.yaml`.
It reads `max_slot_wal_keep_size`, `pg_stat_statements.max`, and `pg_stat_statements.track` from `deploy/gcp/cnpg-cluster.yaml`.

The deployed `event_broker_files` generator supplies separate credentials and subject permissions for the development identity.

It starts Receiving through `wamn dev up` and `wamn dev --hold`, with an operator PAT issued by `wamn-identity`.
It pins the host to cores 0–3, PostgreSQL to cores 4–5, and the client to cores 6–7.
Activation supplies the shared host settings described in [development loop](development-loop.md), including the three allocator residency values from the release chart.
That page also describes the unit test that compares local activation with the rendered deployed host.
The wrapper only records the process ID and pins the host CPU cores.
The default run sends requests through an nginx container using the configuration rendered from `deploy/platform/edge`.
The local listener uses plain HTTP on a free loopback port. The chart owns `/api/` removal and Host handling.
It also owns the single worker and HTTP/1.1 upstream connections without keepalive.
The container uses host networking to reach the loopback host and mounts the local identity CA.
There is no load balancer in this local run.
For a direct comparison, run `tools/bench/local.sh --no-edge --concurrency 16 --duration-secs 60`.
`--no-edge` bypasses nginx and does not require Helm or the nginx image.

The script loads the 1000-order seed and runs get, update, query, record_receipt, and the mix for 60 seconds each at concurrency 16.
The default run writes `tests/bench/local-<CHART_VERSION>.json` in the same format as the platform results.
Other run modes keep their summaries in the private run directory.
The raw results and logs stay in a private directory under `TMPDIR`.
If `TMPDIR` is unset, the script uses `/var/tmp`.
On exit, the script stops its development processes and removes only its own containers and volumes.
Each baseline includes throughput, p50, p95, p99, completed calls, errors, and PostgreSQL statement counts per request.
A `local.sh` result also includes `host_cpu_ms_per_request`. This value is the user and system CPU time of the host process in the timed interval, divided by the completed calls.
The raw results retain guest digests, edge selection, maximum latency, and version conflicts.
Throughput counts all completed attempts, including errors and conflicts.

Run the local regression gate after the benchmarks:

```bash
cargo run --offline -p wamn-conformance-tests --bin wamn-benchmark-gate -- --local
```

The gate compares the newest two local application results and the newest two local PostgreSQL results separately.
It requires concurrency 16 and refuses a throughput drop greater than 20% for any call in the previous result.
Without `--local`, the command compares platform results with the same rules.
Local timings inform design decisions but do not decide them.

Use `--call get`, `--call update`, `--call query`, `--call receipt-query`, `--call create`, or `--call record_receipt` for one call type.
Use `--call app-path` for receipt get with the full fixture and the default edge path.
`create` calls `supplier/create` with a unique name and key.
`record_receipt` calls the base `/receiving/record_receipt` route with one line on a separate seeded order.
Use `--lines <n>` with `record_receipt` to seed each order with n lines, 1 to 100, and to record all n in each receipt.
Command runs reserve 50 orders per worker per second before timing. The default mix reserves 48,000 additional orders.
Each command consumes one order, and the run fails if its seed runs out before the deadline.
`query` filters purchase orders to open status and follows pages of 20 rows.
`receipt-query` follows pages of 20 receipts. The receipt query contract has no filter field.

Use `--mix` for 70% reads, 20% updates, and 10% creates and commands in one run.
Each worker cycles through 20 calls with a different starting offset.
The read weights are 20% purchase-order get, 20% receipt get, 15% purchase-order query, and 15% receipt query.
Supplier create and record receipt each receive 5%.
The result reports every call type and the total.

Use `--contention` to update four shared orders, or combine it with `--mix`.
Use `--call record_receipt --contention` to record receipts against four shared orders.
Each shared receipt order has one line whose quantity covers the full run.
Workers keep independent expected versions, so conflicts trigger recovery reads.
The result counts recovery reads as get calls and reports version conflicts apart from errors.
Recovery reads add traffic beyond the requested mix weights.

Before timing, isolated warm calls measure statements per request from `pg_stat_statements` deltas.
Update contention also calibrates a refused update and weights both statement paths by their observed counts.
Mixed results weight these counts by actual calls.
Calibration includes application and HTTP-admitter roles, with `toplevel=true`.
This includes the WAL-position query used for list ETags.
These are `pg_stat_statements` counts. PostgreSQL does not count each execution of prepared transaction controls. Unexpected errors can affect these estimates.
Connection initialization and cursor cancellation can also affect these estimates.
The result also retains database samples before and after the timed interval.
For a short run, use `--concurrency 4 --duration-secs 10`.

Repeat `--call` to measure several call types on one local database, for example `--call get --call update --call query`.
Each call produces separate statistics. Data persists between calls, so creates and commands increase later list sizes.
Use separate invocations when each call needs a fresh fixture.

Every local run enables statement planning statistics, nested SQL statistics, and PostgreSQL I/O timing.
The benchmark resets `pg_stat_statements` after its seed and calibration calls, immediately before the timed interval.
Each result retains all statement rows before and after that interval, including role, query ID, and top-level status.
The rows include calls, execution and planning times, rows, shared buffers, WAL records, full-page images, and WAL bytes.
The CSV files also retain every column from `pg_stat_statements`.
Do not add nested statement totals to their parent totals. The parent already includes that work.
PostgreSQL also records cursor work under both `FETCH` and its underlying `SELECT`, even when both rows say `toplevel=true`.
Those times overlap. Recorded statement counts describe statistics entries, not physical protocol commands.

The result records `pg_stat_wal`, `pg_stat_checkpointer`, and WAL rows from `pg_stat_io` at both boundaries.
`<call>.database-delta.json` contains the changes. The calculation refuses counters that reset during the run.
These cluster-wide counters include platform background work. PostgreSQL can publish backend counters after a short delay.
PostgreSQL 18 reports WAL write and synchronization times through `pg_stat_io`.

A persistent observer samples `pg_stat_activity` every 100 ms into `<call>.wait-events.log`.
Each sample records the session, role, state, wait event, and query ID.
Use the result's nanosecond timestamps to select samples from the timed interval.
Separate active sessions from idle client waits when calculating wait shares.

`auto_explain` considers all executable statement classes, including nested SQL, with a 1% sample rate.
Seed connections and the benchmark database connection disable sampled plan and duration logging.
The application records analyzed plans, buffers, and WAL at a zero duration threshold. Per-node timing stays off to limit collection cost.
PostgreSQL also samples 1% of transactions and 1% of individual statement durations.
These duration logs cover transaction controls such as `COMMIT`, which have no execution plan.
The runner captures the PostgreSQL container logs in `postgres.log` before removing its own container.

The PostgreSQL floor records the same statistics for each replay and built-in workload.
It also records `pgbench -r` command timings and transaction mean and maximum latency.
Pipeline command timings overlap. Do not add them as independent statement costs.
Use server statement times and sampled commit durations to explain the replay gap. Compare means, not sums of percentiles.

Run `tools/bench/local.sh --pg-floor` to measure PostgreSQL without a host, edge, broker, or collector.
It uses the same PostgreSQL image, base parameters, instrumentation, and CPU assignments as the full local run.
The existing package applier installs Receiving, including its history triggers and generated grants.
The run loads the 1000-order seed and initializes pgbench at scale 10.

The floor requires PostgreSQL 18 `pgbench` and uses its prepared protocol.
It runs select-only, simple-update, get replay, and update replay at concurrency 16 for 60 seconds each.
The replay scripts live in `tools/bench/pg-floor/`.
Their comments describe the captured SQL and parameter choices.
The update replay retains the host transaction, including the write log and logical message.

Each replay includes one extra parameter query before the host statements.
Update clients use separate order slices, so no two clients update the same order.
The finish payload uses a representative row value with its pre-update audit timestamps.
The runner requires one changed row and one finished claim for every completed update.

The floor result is `tests/bench/local-pg-<CHART_VERSION>.json`, in the same format as the platform results.
TPS means completed transactions per second.
The p50 and p99 values come from [pgbench's completed-transaction logs](https://www.postgresql.org/docs/18/pgbench.html), in milliseconds.

Compare select-only and simple-update with the [GCP floor](#postgres-floor) to locate database or storage differences.
Compare the replays with full local get and update to distinguish database work from request-path work.
These timings use different clients, so subtracting their percentiles does not measure host overhead.

Run `tools/bench/local.sh --app-path` to measure the real host and guest path with a fixed, warm database lookup.
This mode loads the size-1 Receiving fixture and seeds one receipt before timing.
It warms the existing `receipt/get` route and repeatedly reads that receipt.
It uses design (a), because the existing route needs less code than an engine benchmark with a substitute PostgreSQL plugin.

It preserves route matching, authentication from memory, component invocation, guest execution, and response handling.
It calls the host directly, with the deployed collector active, for 60 seconds at concurrency 16.
The database still participates, and the result reports the time in its related spans as `postgres_mean_ms`.

The application result is `local-app-<CHART_VERSION>.json` in the private run directory.
It includes throughput, p50, p99, maximum latency, guest digests, and host span parts from the collector's local trace file.
The deployed host keeps sampled parents and samples 10 percent of new traces. The collector does not sample traces.
The result reads that percentage from the measured host and records `sampled_trace_count` for requests inside the measured interval.
The runner stops the host and collector cleanly before it reads the file, so the collector flushes its buffered traces.


Each span part excludes the union of its direct children's time intervals.
`span_parts` groups these own times by span name, as [Named parts](#named-parts) does.
`mean_ms` is the sum divided by the sampled request count, so repeated spans contribute their combined time per request.
`per_span_mean_ms` is the mean for one occurrence, and `span_count` records its sample size.
The reader uses sampled traces directly and does not multiply their times by ten.

Compare this mode with full local get and update to locate changes in the host and guest path.
Use the full run's `--no-edge` variant when comparing direct host latency.
If the Postgres floor changes while the application span parts stay stable, investigate the database layer.
If application span parts change while the floor stays stable, investigate those host or guest parts.
If only the full run changes, inspect its application SQL, payload, and edge behavior.

Run `tools/bench/local.sh --mix --profile` and `tools/bench/local.sh --call update --profile` for function profiles.
The profiling host uses the Dockerfile's packages and features, release optimization, line-table debug information, and frame pointers.
Its Cargo target directory is `/var/tmp/wamn-h2r2l/host-target`, separate from the normal release build.
The script records the binary hash, source revision, working diff, and build duration.
Profiling guests keep function names through `RUSTFLAGS=-C strip=none` and use a separate `host-target/guests` directory.
Their release optimization and features stay unchanged. Their recorded digests differ because their symbol sections remain.

The profiling build sets `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`.
Wamn's engine uses this existing Cargo setting to select Wasmtime's `PerfMap` profiler.
The pinned wash-runtime has no profiling environment switch, so Wamn supplies its existing Wasmtime configuration hook.
Ordinary builds keep the default profiler. The change does not modify upstream source.
The required `/tmp/perf-<pid>.map` is a temporary link to `guest.map` in the private run directory.
The script removes its own link at shutdown.
Later raw-data decoding needs that link again. The saved text reports already contain the guest symbols.

Wasmtime truncates guest names at 96 characters.
The name resolver matches each function index and exact truncated name against the compiled guests, then demangles the full name.
Ambiguous and unmatched names remain unchanged. The resolver records those counts and retains the raw stacks.

Recording starts after seed and calibration work, at the benchmark's start marker.
The script samples user CPU at 99 Hz with `perf record --call-graph fp`.
If more than 10% of stacks are short or contain an unknown frame, it repeats the call with `--call-graph dwarf,16384`.
The repeat has its own timing result. Receipt commands seed new orders for each run.

Self time is CPU time spent inside a function. Inclusive time also includes called functions.
Each profile retains raw data, decoded stacks, a flamegraph, and self-time and inclusive-time reports.
Decoding uses `--no-inline` to retain physical call frames and avoid the local `addr2line` failures during inline expansion.
Its summary lists the top 30 functions, host and guest shares, unresolved samples, and allocation callers.
Generated transition functions count with guest code. CPU percentages use the recorded sample periods.
Inclusive time counts each function once per stack, so inclusive percentages overlap.
Allocation attribution uses CPU samples inside recognized allocation and free functions, then selects the first caller outside those functions.
It measures CPU cost, not allocation counts or bytes, and needs no root probes or extra allocator.

The machine needs `perf`, `wasm-tools`, and `rustfilt` on `PATH`.
It needs `inferno-collapse-perf` and `inferno-flamegraph` in `~/.cargo/bin`.
The script refuses profiling when `/proc/sys/kernel/perf_event_paranoid` exceeds 1.
All profile files stay in the private run directory under `/var/tmp/wamn-h2r2l/`.

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

Commit `6458911fe` sets the guest pool to 16 and the platform pool to 8 in both wamn-dev host groups. The pool `main` has 3 nodes of 2 vCPU. Since `wamn-0371x`, PostgreSQL runs alone on pool `pg` and the Receiving host runs alone on pool `bench` ([gcp](gcp.md#42-cluster)).

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

For get, wash-runtime invoke is the largest part. For update, the item commit and the application statement are larger.

## Postgres floor

pgbench ran in a pod in the cluster against `wamn-pg-1`, at scale 10 with 16 clients for 60 s.

| Script | TPS | p50 | p99 |
| --- | --- | --- | --- |
| select-only | 10377 | 0.34 | 6.36 |
| simple-update | 1888 | 6.88 | 23.04 |

The item commit p50 of update (8.42 ms) is near the simple-update p50 of pgbench (6.88 ms), which also ends in a commit.

### Update gap on GCP

Epic `wamn-c8a5v`, issue `wamn-c8a5v.1` ([plan](../plan/perf-arch.md), M4 and M5).
`tools/bench/pg-floor/gcp.sh` runs the replays of `local.sh --pg-floor` against `wamn-pg-1`.
The result is [`tests/bench/gcp-pg-0.1.36.json`](../../tests/bench/gcp-pg-0.1.36.json).
The gap is the update replay p50 minus the simple-update p50, at c16 for 60 s each.
The row "update, triggers off" is the update replay with both record-history triggers disabled (M4).
It ran after the update replay on the same data, with about 35,000 more write log and history rows, so the trigger share is a low estimate.

| Run | simple-update | update replay | Gap | update, triggers off | Trigger share of the gap |
| --- | --- | --- | --- | --- | --- |
| Local 0.1.28, [`local-pg-0.1.28.json`](../../tests/bench/local-pg-0.1.28.json) | 2489/s, p50 6.20 | 1386/s, p50 11.58 | 5.39 ms | not measured | not measured |
| GCP tier 2, 0.1.36, 2026-10-20 12:12–12:18 UTC | 2319/s, p50 6.35 | 585/s, p50 26.02 | 19.66 ms | 1110/s, p50 13.46 | 12.55 ms, 64% |
| GCP tier 2, 0.1.36 with `wamn-c8a5v.2`, 2026-10-20 14:29–14:34 UTC, [`gcp-pg-0.1.36-c8a5v.2.json`](../../tests/bench/gcp-pg-0.1.36-c8a5v.2.json) | 2039/s, p50 7.02 | 668/s, p50 20.94 | 13.93 ms | 1033/s, p50 14.55 | 6.40 ms, 46% |

The GCP run used pool `pg` for PostgreSQL alone, on an `e2-standard-2` node.
The pgbench client ran in the Job `tools/bench/pg-floor/gcp-client.yaml` on pool `bench`, beside the idle Receiving host.
The platform pods on pool `main` were present. During the run, the other databases on `wamn-pg-1` committed about 8 transactions per second.
The Cluster restart that loaded `auto_explain` ended at 11:28:13 UTC. Nothing else ran against the cluster between the restart and the runs.

The runner installs Receiving into its own database `wamn_pg_floor` with `pg_floor_database`.
That database carries the local statement statistics: `pg_stat_statements.track = all`, `track_planning`, `track_io_timing`, `track_wal_io_timing`, and 1% transaction and statement sampling.
The environment databases do not carry these settings. They keep the Cluster's `track = top`, so the calls per request of `tier.sh` stay comparable.
During this run, the Cluster set the `auto_explain` values of `local.sh` for every database.
After this run, the Cluster keeps `auto_explain` loaded but silent (`log_min_duration = -1`), and `gcp.sh` sets those values on `wamn_pg_floor` alone.
The kubelet rotates the PostgreSQL container log, so the run kept only 378 analyzed plans. The split below uses `pg_stat_statements`.

The split uses means, because pipeline command timings overlap. The mean gap is 20.45 ms (27.35 ms against 6.90 ms).

| Share | Mean latency | Server time per transaction | WAL per transaction | Source |
| --- | --- | --- | --- | --- |
| Triggers | 12.94 ms | 6.78 ms | 1,176 bytes, and 2,389 against 9 full-page images in total | M4 latency difference |
| Write log, claim and finish | part of the rest | 0.42 ms | 987 bytes | `pg_stat_statements`, triggers off |
| Logical message | part of the rest | 0.02 ms | 149 bytes | `pg_stat_statements`, triggers off |
| Rest | 7.51 ms, write log and message included | 0.62 ms | 407 bytes | remainder |

Without triggers, the server spends 1.06 ms per transaction on top-level statements, so most of the rest is waiting.
That waiting covers the four round trips, the commit flush and CPU queueing of 16 clients on 2 vCPUs.
The commit trip has a mean of 8.07 ms, against 3.44 ms for the `END` of simple-update.
The replay's extra parameter query is part of the rest, with a mean of 1.65 ms.

With triggers, the update command statement takes 7.21 ms of server time against 0.41 ms.
Inside the triggers, the dynamic history `INSERT` is planned again for each row: 35,012 plans of 0.23 ms for 35,012 updates. Its execution takes 1.23 ms.
The catalog queries are planned once per session and run for each row. `row_image` runs twice at 0.28 ms, and the primary key query takes 0.32 ms.
The `jsonb_each` diff takes 0.27 ms, the tenant lookup 0.13 ms, and the stamp write-back 0.11 ms.
Every trigger and command statement ran as `wamn_app`. The foreign key check ran as `wamn_db_owner`.

The third row is the same procedure after `wamn-c8a5v.2`, on new nodes of the same pools.
The package relations then run generated trigger functions with static SQL, and their images keep the `to_jsonb` spelling.
In that run, the triggers cost 6.40 ms at p50 against 12.55 ms, and 8.45 ms against 12.94 ms in the mean.
Their server time per update fell from 6.78 ms to 2.63 ms, and the nested plans fell from 36,212 to 720.
Update throughput rose from 585/s to 668/s. Simple-update fell from 2319/s to 2039/s on the new nodes, so compare the rows within each run.
Before this run, the Cluster kept `auto_explain` silent outside `wamn_pg_floor`, and the other databases committed about 6 transactions per second.

### Group commit and rows per command on GCP

Issue `wamn-c8a5v.3` ([plan](../plan/perf-arch.md), M6 and M7) used the venue of the update gap: GCP tier 2, PostgreSQL alone on pool `pg`, and the pgbench Job on pool `bench`.
The source was `215029220` with the `gcp.sh` changes of this issue, on platform 0.1.36, on 2026-10-20 between 15:17 and 15:51 UTC.
Every run lasted 60 s and had 0 failed transactions.

M6 sets `commit_delay = 1000` and `commit_siblings = 5` on `wamn_pg_floor` alone, with `COMMIT_DELAY=1000 tools/bench/pg-floor/gcp.sh`.
Each row is one complete `gcp.sh` run on a new database. The result files are `tests/bench/gcp-pg-0.1.36-m6-c<n>-<off|1000>.json`.

| Concurrency | `commit_delay` | simple-update | update replay | update, triggers off | WAL fsyncs in the update replay |
| --- | --- | --- | --- | --- | --- |
| c16 | 0 | 2209/s, p50 6.61 | 842/s, p50 17.08, p99 48.53 | 1069/s, p50 13.89 | 13,595 for 50,417 updates |
| c16 | 1000 µs | 2019/s, p50 7.45 | 809/s, p50 18.57, p99 45.71 | 994/s, p50 14.96 | 9,482 for 48,515 updates |
| c4 | 0 | 946/s, p50 4.03 | 613/s, p50 6.11, p99 13.16 | 712/s, p50 5.27 | not compared |
| c4 | 1000 µs | 945/s, p50 4.02 | 590/s, p50 5.94, p99 25.67 | 737/s, p50 5.11 | not compared |

At c16 the delay cut the WAL fsyncs by 30%, but update throughput fell by 4% and the p50 rose by 1.5 ms.
At c4 the differences are within one run's spread, except a higher update p99.
The measurement does not adopt the setting. That decision is open decision 5 of the plan.

M7 ran as a floor replay, not through the host.
`CALLS="receipt-1 receipt-5 receipt-20" tools/bench/pg-floor/gcp.sh` replays `record_receipt` with 1, 5 and 20 lines per receipt at c16.
The replay `tools/bench/pg-floor/receipt.sql` follows the statement sequence of a local `wamn-bench` run (`f025f5876`, 2026-10-18), and `gcp.sh` puts the current command SQL into it.
`receipt-slots.sql` reserves the orders and lines of each client on the floor seed, so no receipt exceeds a remaining quantity.
The receipt calls set `track_functions = pl`, and the trigger time is the `pg_stat_user_functions` time of the generated `receiving__*` functions.
The statement time is the `pg_stat_statements` execution time of the `record_receipt` statement, triggers included.
The result file is [`tests/bench/gcp-pg-0.1.36-m7.json`](../../tests/bench/gcp-pg-0.1.36-m7.json).

| Lines per receipt | Receipts | p50 | Statement time per row | Trigger time per row | Trigger share |
| --- | --- | --- | --- | --- | --- |
| 1 | 390/s | 35.33 ms | 13.27 ms | 6.61 ms | 50% |
| 5 | 218/s | 68.74 ms | 5.70 ms | 2.42 ms | 42% |
| 20 | 133/s | 111.53 ms | 3.82 ms | 1.88 ms | 49% |

The per-row trigger cost falls with more lines, because the order triggers fire once for each receipt.
The line trigger `receiving__purchase_order_line_log` stays at about 1.7 ms for each line at 5 and 20 lines.
The triggers keep about half of the statement time at every line count.
The 16 clients saturate the 2 vCPUs: most sampled sessions had no wait event, so the times include CPU queueing.

## Request round-trips

Epic `wamn-stxy9` ([plan](../plan/request-round-trips.md)).
These runs reset `pg_stat_statements` before each run and saved every row afterward in per-run CSV files.
Git retains those files.
The current [procedure](gcp.md#58-benchmark) stores the calls per request with each platform result.

| Run | Placement | get | update | get calls | update calls |
| --- | --- | --- | --- | --- | --- |
| 0.1.19, 12:41 | main 3 nodes, Postgres shares a node | 427/s, p50 31.0 | 159/s, p50 81.1 | 7 | 15 |
| 0.1.21, 13:16, `wamn-stxy9.1` | main 2 nodes, Postgres alone on pool pg | 360/s, p50 37.3 | 276/s, p50 49.7 | 6 | 14 |
| 0.1.21, 14:07, `wamn-0371x` | Receiving host alone on pool bench, Postgres alone on pool pg | 632/s, p50 22.6 | 284/s, p50 46.3 | 6 | 14 |
| 0.1.23, 18:42, `wamn-stxy9.2` | Receiving host alone on pool bench, Postgres alone on pool pg | 645/s, p50 21.8 | 376/s, p50 36.3 | 2 | 10 |
| 0.1.23, 19:48, warm reuse pool 16 | Receiving host alone on pool bench, Postgres alone on pool pg | 855/s, p50 15.2 | 335/s, p50 41.1 | 2 | 10 |
| 0.1.25, 21:39, `wamn-stxy9.3` | Receiving host alone on pool bench, Postgres alone on pool pg | 822/s, p50 16.2 | 415/s, p50 34.8 | 1 | 7 |

### Host profile, 0.1.25

`wamn-xqxoa`. The run used `perf record -F 99 --call-graph dwarf` on the Receiving host for 60 s of get c16 (822/s), from a node debug pod on pool `bench`. The host used 1.64 CPUs under `perf`. The [flamegraph](../../tests/bench/flamegraph-get-c16-0.1.25.svg) holds the stacks. No symbol takes more than 2.5% except the two groups that `perf` cannot name, and the top 10 hold 35% of the samples.

| Self time | Symbol |
| --- | --- |
| 13.86% | libc, no symbols (the debug pod libc is a different build) |
| 8.90% | guest wasm code (`perf-<pid>.map`) |
| 2.41% | `malloc` |
| 2.36% | `free` |
| 1.95% | `handle_softirqs` (network receive) |
| 1.42% | `smp_call_function_many_cond` (TLB shootdown) |
| 1.06% | `_raw_spin_unlock_irqrestore` |
| 0.86% | `native_write_msr` |
| 0.81% | `serde_json::Value::eq` |
| 0.70% | `nft_do_chain` (packet filter) |

During update c16, `pg_stat_activity` showed no lock wait. The update sessions waited on the host (`ClientRead`), on `WALWrite` and `WalSync` at commit, and on CPU. The bench gives each worker its own orders, so the 5.6 ms update statement is not one hot row.

Issue 1 deleted the actor label query. Each call makes one call fewer, and the own time of `wamn.route.deliver` fell from 4.0 ms to 0.15 ms. The two runs differ in placement too. In the 0.1.21 run, both hosts, the collector and the event NATS share one 2-vCPU node, and the get host p50 rose to 31.7 ms. The collector restarted 11 times with `OOMKilled` (`wamn-kclnp`), so the 0.1.21 traces hold 2091 of 21636 get requests and no update request.

In the 0.1.19 run, `wamn_cache.bump_version` took 20.2 ms mean per update, and the update statement 15.0 ms. With Postgres alone on its node, they took 2.1 ms and 3.0 ms.

The 14:07 run is the clean row before issue 2. The Receiving host runs alone on pool `bench`, and every other pod runs on the one node of pool `main` (`wamn-0371x`). The collector did not restart (`wamn-kclnp`), so the traces hold 30771 of 38197 get requests and 16413 of 18121 update requests. The host p50 (`handle_http_request`) is 14.3 ms for get and 39.3 ms for update.

| Span, p50 ms | get | update |
| --- | --- | --- |
| `wamn.route.authenticate` | 3.6 | 4.2 |
| `wamn.auth.identity` | 1.8 | 2.1 |
| `wamn.auth.permissions` | 1.6 | 1.8 |
| `wamn.postgres.session_settings` | 1.4 | |
| `wamn.postgres.statement` | 1.5 | 6.0 |
| `wamn.operation.begin_item` | | 4.2 |
| `wamn.write_log.claim` | | 3.8 |
| `wamn.write_log.finish` | | 4.5 |
| `wamn.operation.commit` | | 7.6 |
| `wamn.route.deliver` | 6.0 | 29.4 |

Issue 2 moved auth into the memory of the host. A get makes 2 statements per request: the claim settings and the read. An update makes 10. The auth queries ran 124 times in the get run of 38710 requests, because a fact lives 60 s. Update throughput rose from 284/s to 376/s, and the update p50 fell from 46.3 ms to 36.3 ms. The collector did not restart, so the traces hold 19458 of 38710 get requests and 3879 of 22604 update requests. The host p50 (`handle_http_request`) is 9.6 ms for get and 24.2 ms for update.

| Span, p50 ms, 0.1.23 | get | update |
| --- | --- | --- |
| `wamn.route.authenticate` | 0.02 | 0.02 |
| `wamn.auth.permissions` | 0.01 | 0.01 |
| `wamn.postgres.session_settings` | 2.2 | |
| `wamn.postgres.bind_claims` | | 1.4 |
| `wamn.postgres.statement` | 1.8 | 4.3 |
| `wamn.operation.begin_item` | | 1.5 |
| `wamn.write_log.claim` | | 2.5 |
| `wamn.write_log.finish` | | 3.0 |
| `wamn.operation.commit` | | 6.7 |
| `wamn.route.deliver` | 8.8 | 22.8 |

## Composed entry, A5

Issue `wamn-l5sus.3` ([plan](../plan/perf-arch.md), A5) moved base routes into the composed entry at 0.1.81.
The paired local run compares the parent `58681131a` (0.1.80) with `4de455732` (0.1.81) plus the `wamn dev up` fix.
It ran `local.sh --no-edge --call get --call update --call query` four times, in the order parent, A5, A5, parent, on 2026-10-22 from 18:06 to 18:24 UTC.
The other agents held their local work, and the load average was 7 to 9.
The local edge does not start on 0.1.80 or later, because its nginx configuration names `wamn-edge-web`.
The results are in [`tests/bench/a5-paired`](../../tests/bench/a5-paired).

| c16, mean of 2 runs | parent 0.1.80 | A5 0.1.81 | A5 / parent |
| --- | --- | --- | --- |
| get throughput | 2888/s | 2278/s | 0.79 |
| get p50, p99 | 5.04 ms, 16.2 ms | 6.50 ms, 18.6 ms | 1.29, 1.15 |
| get statements per request | 1 | 2 | 2.00 |
| get `host_cpu_ms_per_request` | 1.24 | 1.52 | 1.23 |
| update throughput | 1122/s | 1120/s | 1.00 |
| update p50, p99 | 13.98 ms, 23.8 ms | 14.08 ms, 23.7 ms | 1.01, 0.99 |
| update statements per request | 7 | 6 | 0.86 |
| update `host_cpu_ms_per_request` | 1.82 | 1.78 | 0.98 |
| query throughput | 1763/s | 1300/s | 0.74 |
| query p50, p99 | 8.76 ms, 16.9 ms | 11.98 ms, 21.1 ms | 1.37, 1.25 |
| query statements per request | 2 | 3 | 1.50 |
| query `host_cpu_ms_per_request` | 2.18 | 2.92 | 1.34 |

Each pair of runs on one side differs by less than 3% in throughput, and no run has errors.
Update is unchanged, and it makes one statement fewer per request.
Get and query now open an operation for each read, so the session settings statement runs once per request.
That statement is `set_config('statement_timeout', ...)` with `search_path`, at 0.015 ms mean execution time.
The read cost is the extra round-trip and the guest work, and `wamn-cg5e4` holds the large stream case.

The `wamn-5x0tt` live test on 2026-10-22 measured the revised composed read path against PostgreSQL 18.
It warmed all three routes, reset `pg_stat_statements`, and sent ten requests per route.
Get used ten SQL calls, or 1 call per request.
Query used twenty SQL calls, or 2 calls per request: one application read and one `pg_current_wal_lsn()` read.
The `/widget_maker/list` projection also used twenty SQL calls: ten application reads and ten WAL reads.
The test counted every statement from the guest database roles, including transaction and configuration statements.
The test is `route_interface_live::composed_reads_use_one_statement_plus_optional_wal` in `tests/integration`.

Trips per request remain unmeasured for all three routes.
The host queues the optional WAL read before the application read in one pipeline on the same connection.
The sampler in `tools/bench/db-profile.sh` samples `pg_stat_activity` every 100 ms and cannot count individual trips.
Use protocol tracing of isolated, warmed requests to count client waits between requests and server responses.
For the next throughput comparison, run `tools/bench/local.sh --no-edge --call get --call query`.
Retain its wait-event logs and statement CSV files.
No new throughput benchmark ran for this change.

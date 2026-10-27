# Benchmark

This page holds the current baselines of the Receiving benchmark: the tier 2 run on `wamn-dev` with the collector in place, the PostgreSQL floor, and the local run.
The procedure and regression gate of the tier 2 run are in [Google Cloud operations](gcp.md#58-benchmark).
`tools/bench/tier.sh` writes one result per platform version to `tests/bench/platform-<version>.json`.
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
The default run writes `tests/bench/local-<version>.json` in the same format as the platform results.
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

The floor result is `tests/bench/local-pg-<version>.json`, in the same format as the platform results.
TPS means completed transactions per second.
The p50 and p99 values come from [pgbench's completed-transaction logs](https://www.postgresql.org/docs/18/pgbench.html), in milliseconds.

Compare select-only and simple-update with the [GCP floor](#postgres-floor) to locate database or storage differences.
Compare the replays with full local get and update to distinguish database work from request-path work.
These timings use different clients, so subtracting their percentiles does not measure host overhead.

Run `tools/bench/local.sh --app-path` to measure the real host and guest path with a fixed, warm database lookup.
This mode loads the size-1 Receiving fixture and seeds one receipt before timing.
It warms the existing `receipt/get` route and repeatedly reads that receipt.
It preserves route matching, authentication from memory, component invocation, guest execution, and response handling.
It calls the host directly, with the deployed collector active, for 60 seconds at concurrency 16.
The database still participates, and the result reports the time in its related spans as `postgres_mean_ms`.

The application result is `local-app-<version>.json` in the private run directory.
It includes throughput, p50, p99, maximum latency, guest digests, and host span parts from the collector's local trace file.
The deployed host keeps sampled parents and samples 10 percent of new traces. The collector does not sample traces.
The result reads that percentage from the measured host and records `sampled_trace_count` for requests inside the measured interval.
The runner stops the host and collector cleanly before it reads the file, so the collector flushes its buffered traces.


Each span part excludes the union of its direct children's time intervals.
`span_parts` groups these own times by span name.
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

## Platform baseline

The newest tier 2 result is [`platform-0.1.26.json`](../../tests/bench/platform-0.1.26.json).
Receiving ran on `wamn-dev` with every host exporting spans to the collector. The run used concurrency 16 for 60 seconds per call and had 0 errors.
The conformance test `benchmark::platform_benchmark_gate` compares the newest two result files.

| Call | Throughput | p50 | p95 | p99 | Statements per request |
| --- | --- | --- | --- | --- | --- |
| get | 838/s | 17.27 | 28.06 | 43.07 | 1 |
| update | 495/s | 28.91 | 46.60 | 65.58 | 5 |

## Local baseline

The newest local result is [`local-0.1.102.json`](../../tests/bench/local-0.1.102.json). It ran on main 9c15c5bf0 (platform 0.1.102) with `local.sh --call get --call update --no-edge`, at concurrency 16 for 60 seconds per call, with 0 errors.

| Call | Throughput | p50 | p95 | p99 | Statements per request | Host CPU ms per request |
| --- | --- | --- | --- | --- | --- | --- |
| get | 2010/s | 6.86 | 16.83 | 25.30 | 1 | 1.51 |
| update | 815/s | 18.16 | 32.33 | 47.07 | 7 | 2.22 |

## Postgres floor

`tools/bench/pg-floor/gcp.sh` runs the replays of `local.sh --pg-floor` against `wamn-pg-1` on GCP tier 2.
PostgreSQL runs alone on pool `pg`. The pgbench client runs in the Job `tools/bench/pg-floor/gcp-client.yaml` on pool `bench`.
The runner installs Receiving into its own database `wamn_pg_floor`. The environment databases keep the Cluster statistics settings.
The newest result is [`gcp-pg-0.1.36-c8a5v.2.json`](../history/evidence/bench/gcp-pg-0.1.36-c8a5v.2.json). Each call ran at concurrency 16 for 60 seconds with 0 errors.
The row "update, triggers off" is the update replay with both record-history triggers disabled. It ran after the update replay on the same data.

| Script | TPS | p50 | p95 | p99 |
| --- | --- | --- | --- | --- |
| select-only | 12761 | 1.02 | 2.77 | 4.48 |
| simple-update | 2039 | 7.02 | 13.66 | 20.66 |
| get replay | 6219 | 2.22 | 4.96 | 7.56 |
| update replay | 668 | 20.94 | 45.18 | 69.61 |
| update, triggers off | 1033 | 14.55 | 24.77 | 36.14 |

The update replay p50 is 13.93 ms above the simple-update p50. Disabling the triggers removes 6.40 ms of that gap at p50.

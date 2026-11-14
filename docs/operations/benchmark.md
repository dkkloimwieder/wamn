# Benchmark

This page holds the current baselines of the Receiving benchmark: the tier 2 run on `wamn-dev` with the collector in place, the PostgreSQL minimum, and the local run.
The procedure for the tier 2 run is in [Google Cloud operations](gcp.md#58-benchmark).
`cargo xtask bench-tier` writes each result in its printed temporary directory.
Latency is in milliseconds.

## Development measurements

Use the installed `kind` platform for development measurements.
[Development loop](development-loop.md#measure-one-change) separates build, publication, and deployment times.
Use the deployed benchmark for request latency and throughput.
The native development benchmark is removed with the native host topology.

## Platform baseline

The newest tier 2 result is [`platform-0.1.26.json`](../../tests/bench/platform-0.1.26.json).
Receiving ran on `wamn-dev` with every host exporting spans to the collector. The run used concurrency 16 for 60 seconds per call and had 0 errors.

| Call | Throughput | p50 | p95 | p99 | Statements per request |
| --- | --- | --- | --- | --- | --- |
| get | 838/s | 17.27 | 28.06 | 43.07 | 1 |
| update | 495/s | 28.91 | 46.60 | 65.58 | 5 |

## Local baseline

The newest local result is [`local-0.1.102.json`](../../tests/bench/local-0.1.102.json). It ran on main 9c15c5bf0 (platform 0.1.102) with `cargo xtask bench-local --call get --call update --no-edge`, at concurrency 16 for 60 seconds per call, with 0 errors.

| Call | Throughput | p50 | p95 | p99 | Statements per request | Host CPU ms per request |
| --- | --- | --- | --- | --- | --- | --- |
| get | 2010/s | 6.86 | 16.83 | 25.30 | 1 | 1.51 |
| update | 815/s | 18.16 | 32.33 | 47.07 | 7 | 2.22 |

## Postgres minimum

`cargo xtask bench-pg-minimum` runs the replays of `cargo xtask bench-local --pg-minimum` against `wamn-pg-1` on GCP tier 2.
PostgreSQL runs alone on pool `pg`. The pgbench client runs in the Job `xtask/bench/pg-minimum/gcp-client.yaml` on pool `bench`.
The runner installs Receiving into its own database `wamn_pg_minimum`. The environment databases keep the Cluster statistics settings.
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

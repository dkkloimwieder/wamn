# Fix 6 — throughput: concurrency 1 to 64 across three layers, the knee

**Source commit:** `94c30a8a` on main (authored on `perf/residue-spans` off `bff98357`)  
**Measured:** 2026-09-05, launched at load 2.30, 3.33, 3.87  
**Data:** `docs/perf/2026.09/6-throughput/` — `journey/throughput/` holds the sweep: per step the generator's own output, a counter sample before and after, four `cpu.stat` snapshots; `index.json`, `report.md`, `summary.json`  
**Bead:** `wamn-0h0g.17.27`

Every number before this report is one request at a time. This asks how many at
once, and where p99 turns. Owner-ruled shape: a stock load generator in a pod,
pinned by digest — `oha` 1.16.0 for the HTTP layers, `pgbench` 18.6 from the pinned
`postgres` image for the direct layer — with Rust doing only the PostgreSQL counter
sampling and the report. No absolute number is asserted; the knee and the peak are
recorded here so the next run is compared to this one.

## The sweep

Concurrency 1, 4, 8, 16, 32, 64; ten seconds a step; closed loop (each client sends
its next request when the previous one answers); HTTP/1.1 keep-alive; the caller a
pod in the same cluster. One host pod (the measure-startup deployment: **CPU limit 2,
request 250m**, from `deploy/platform/values-host-receiving-pat.yaml`), one
PostgreSQL 18 container and one NATS container on the same eight-core machine,
beside the three kind nodes and the generator itself. The host is the gate-of-record
**debug** image.

| layer | driver | what it measures |
|---|---|---|
| `route` | oha, `POST /purchase_order/get`, authenticated | the route end-to-end: auth (system DB), the flow guest, the data-access guest, the statement (project DB), two events |
| `nodb` | oha, `GET /no-such-route` | the host's per-request path and the flow guest's dispatch, answered 404 by the guest: no auth, no database |
| `pg` | pgbench `-M prepared`, the generated `purchase_order/get` read, schema-qualified, as `postgres` | the statement alone, from a direct client on the pod network |

## Knee and peak

| layer | knee (last step that scaled) | p99 turns at | throughput there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **4** | 8 (44 → 94 ms) | ×0.91 | 162 | 64 | 462 ms |
| `nodb` | **4** | 8 (11.7 → 26 ms) | ×0.91 | 698 | 32 | 72 ms |
| `pg` | **8** | 16 (0.25 → 1.42 ms) | ×0.92 | 79,383 | 8 | 0.25 ms |

The knee is the last step whose successor still gained at least 20 % in throughput;
the successor is where p99 turned. On every layer the turn is where throughput went
flat and p99 doubled or more in the same step, so the rule and the picture agree.

## What saturates first: the host's CPU quota

**The route knees at four clients and about 150 req/s because the host pod is out
of CPU at two cores. Nothing else is busy.**

| at the route's knee and beyond | reading |
|---|---|
| host CPU periods throttled at the quota | 44 % at c=1, **52–68 %** from c=4 on |
| host CPU during the ten seconds of load | ≈2.2 cores (1.6 averaged over the 13.8 s sample window, which includes the Job's scheduling at idle); 0.04 cores between steps |
| PostgreSQL | 0.06–0.07 cores; 470–590 commits/s, about 3.5 per request |
| guest SQL pool (max 14) | 13 backends open from c=32, **all idle** at the end of every step |
| platform pool (max 2) | 2 backends, one active |
| NATS | 2 messages per request, 0 CPU, one connection, no slow consumers |
| per-route in-flight cap (64) | never exceeded: zero errors on every step, the only non-answers are the one-per-client requests the deadline cut off |

The no-DB layer says the same thing without a database in the picture: 239 req/s
single-stream at 4 ms, a knee at c=4 (611 req/s), flat to 700, throttled 48–66 %.

**Host CPU per request** — the pod's CPU seconds across the step over the requests it
answered — is the number the ceiling is made of:

| layer | c=1 | c=4 | c≥8 |
|---|---:|---:|---:|
| `route` | 32.6 ms | 15.3 ms | 14.5–17.0 ms |
| `nodb` (a 404) | 9.2 ms | 3.6 ms | 3.1–3.9 ms |

At two cores, 14–17 ms of CPU per request is 120–140 req/s, which is the plateau.
The fall from c=1 to c=4 is not idle burn (0.04 cores between steps): roughly a core
of the CPU under load is spent per unit of time rather than per request. Candidates
are the OTel exporter, runtime wakeups and the epoch ticker; none is measured here
(`wamn-0h0g.17.31`).

**The two-core limit is the overlay's, not the default's.** `values-host-default.yaml`
sets 6 with a comment titled "WHY 6 AND NOT 2", measured on `wamn-0h0g.17.23`; the
Receiving overlay the journey deploys sets 2, so every measurement in this directory
ran under the cap that comment says it removed, `4-instantiate` and the unexplained
in-cluster multiplier included (`wamn-0h0g.17.30`).

## The direct statement

The statement alone: 22,600 tps from one client at p50 0.04 ms; knee at c=8,
**79,400 tps** at p99 0.25 ms with PostgreSQL at 2.9 cores over the window (about
4.4 during load); past it throughput slips and p99 climbs to 6 ms at c=64 — the
eight-core box, shared with the generator and the kind nodes, is what runs out. The
route delivers 150 req/s where its statement delivers 79,000: the platform costs the
statement 500× in throughput and 350× in single-stream latency (14.4 ms against
0.04 ms), and the whole of that cost is host CPU.

## Per layer

### `route` — oha against `POST /purchase_order/get`, authenticated

| c | req/s | p50 ms | p99 ms | errors | cut off | answered | server commits/s | backends | host cores (window) | host CPU ms/req | throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 68 | 14.38 | 20.80 | 0 | 1 | 676 | 171 | postgres=2 project=4 | 1.22 | 32.6 | 44 % | 0.02 |
| 4 | 145 | 26.83 | 43.98 | 0 | 4 | 1446 | 510 | postgres=2 project=8 | 1.59 | 15.3 | 68 % | 0.06 |
| 8 | 131 | 59.46 | 94.18 | 0 | 8 | 1306 | 470 | postgres=2 project=10 | 1.61 | 17.0 | 55 % | 0.06 |
| 16 | 152 | 104.15 | 148.94 | 0 | 16 | 1508 | 542 | postgres=2 project=10 | 1.59 | 14.5 | 62 % | 0.07 |
| 32 | 142 | 223.89 | 325.06 | 0 | 32 | 1386 | 524 | postgres=2 project=13 | 1.60 | 15.9 | 52 % | 0.07 |
| 64 | 162 | 402.59 | 461.63 | 0 | 64 | 1555 | 593 | postgres=2 project=13 | 1.64 | 14.5 | 51 % | 0.07 |

### `nodb` — oha against `GET /no-such-route`, answered 404 by the guest

| c | req/s | p50 ms | p99 ms | errors | cut off | answered | host cores (window) | host CPU ms/req | throttled |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 239 | 3.97 | 8.11 | 0 | 1 | 2387 | 1.58 | 9.2 | 48 % |
| 4 | 611 | 6.26 | 11.68 | 0 | 4 | 6111 | 1.60 | 3.6 | 65 % |
| 8 | 557 | 14.52 | 26.03 | 0 | 8 | 5560 | 1.57 | 3.9 | 66 % |
| 16 | 663 | 24.21 | 42.89 | 0 | 16 | 6619 | 1.59 | 3.3 | 49 % |
| 32 | 698 | 46.31 | 71.83 | 0 | 32 | 6948 | 1.58 | 3.1 | 64 % |
| 64 | 674 | 99.42 | 151.12 | 0 | 64 | 6673 | 1.58 | 3.3 | 62 % |

### `pg` — pgbench, the generated `purchase_order/get` read, direct

| c | tps | p50 ms | p99 ms | failed | transactions | server commits/s (window) | pg cores (window) |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 22,584 | 0.04 | 0.10 | 0 | 225,680 | 10,647 | 0.32 |
| 4 | 47,682 | 0.07 | 0.19 | 0 | 476,170 | 33,452 | 1.79 |
| 8 | 79,383 | 0.08 | 0.25 | 0 | 792,019 | 51,727 | 2.86 |
| 16 | 72,948 | 0.08 | 1.42 | 0 | 726,980 | 49,700 | 2.86 |
| 32 | 69,164 | 0.26 | 2.62 | 0 | 686,832 | 45,999 | 2.83 |
| 64 | 71,323 | 0.43 | 6.21 | 0 | 704,290 | 47,506 | 3.02 |

Server commits/s and cores are over the 14–21 s sample window (the step plus the
Job's scheduling), so they read lower than the generator's ten-second rate; the
generator's numbers are the step's.

## What this changes

- **The overhead-ratio gate read green on this run**, 6.71 against the ceiling 9,
  on the same host that had read 10.55 the run before (`wamn-0h0g.17.29` stands).
- The throughput target is host CPU per request, not any queue: the route needs
  15–33 ms of a debug host's CPU, and the 404 path 3–9 ms, before a single
  millisecond of database time. Whether the knee is to be read on a release-profile
  host, and what the non-proportional core is, are owner calls (`wamn-0h0g.17.31`).
- Re-sweep after `wamn-0h0g.17.30` is ruled, so the knee reads the product and not
  the quota.

## Method

`tools/receiving-cluster-journey-run --apply --throughput` from the lane; the sweep
runs after the steady request and before the ratio gate. Per step the journey renders
one Job (`tools/journey-throughput.sh`, digest-pinned images, the PAT reaching oha
through Kubernetes' own `$(VAR)` expansion), samples `pg_stat_database`,
`pg_stat_activity` and NATS `/varz` before and after (`wamn-throughput sample`),
snapshots the host pod's and the PostgreSQL container's cgroup `cpu.stat`, and keeps
the generator's own output. `wamn-throughput report` reduces it: req/s and p50/p99
are the generator's; commits and backends are PostgreSQL's own counters; cores,
CPU per request and the throttled share are `cpu.stat` deltas across the sample
window. The shape gate is the ignored test `throughput_bench_live`, run over
`journey/throughput/` (green). pgbench's per-transaction log was sampled at 20 %
this run and is 21 MB of the evidence; the journey now samples at 5 %.

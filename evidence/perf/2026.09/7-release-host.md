# Fix 7 — the release host on the default 6-core cap: the knee, re-measured

**Source commit:** `abf85717` on main (authored on `perf/residue-spans` off `7ca2eddc`)  
**Measured:** 2026-09-05, launched at load 1.82, 4.69, 6.26  
**Data:** `docs/perf/2026.09/7-release-host/` (same layout as `6-throughput/`; five steady traces)  
**Beads:** `wamn-0h0g.17.31` (first half), `wamn-0h0g.17.30`, `wamn-0h0g.17.29`

Three owner-ordered changes to the measurement, then the same sweep as `6-throughput.md`:

1. **The host is a release build.** The journey had built `wamn-host` without
   `--release` and copied `target/debug` into its image, labelled `debug`, for every
   report in this directory. The two-stage Dockerfile always built the host with
   `--release`; only the journey's path did not — the guest-profile defect of
   `4-instantiate.md`, one layer down. Confirmed on this run: `Compiling wamn-host`
   under the release profile, 11 m 29 s cold; the binary 90 MiB against 421; the image
   188 MB against 518, labelled `release`.
2. **The cap is the default's.** `values-host-receiving-pat.yaml`, the file the journey
   deploys, kept `250m/2` from the day it was cut (`84af9ebc`) while `2fff2c03` raised
   only the default to `2/6`; now aligned, the divergence recorded in the file.
3. **The ratio gate asserts the median of five steady requests** instead of one.

## Before and after

| | `6-throughput` (debug host, 2 cores) | `7-release-host` (release host, 6 cores) |
|---|---:|---:|
| in-host request, steady (`handle_http_request`, median of 5) | 12.8 ms | **4.4 ms** (4.30, 4.36, 4.40, 4.42, 6.51) |
| route, single stream | 68 req/s, p50 14.4 ms | **263 req/s, p50 3.4 ms** |
| route, knee | c=4, 145 req/s | **c=8, 1,543 req/s** |
| route, peak | 162 req/s at c=64, p99 462 ms | **1,652 req/s at c=16, p99 17 ms** |
| host CPU per route request (c=1 → plateau) | 32.6 → 14.5 ms | **5.6 → 2.6 ms** |
| host throttled at the knee | 68 % | **0 %** (3.1 cores of 6 at peak) |
| no-DB route, single stream / peak | 239 req/s / 698 req/s | **1,934 req/s / 12,897 req/s** at c=32 |
| host CPU per 404 (c=1 → plateau) | 9.2 → 3.1 ms | **0.8 → 0.4 ms** |
| statement direct, peak | 79,400 tps at c=8 | 78,800 tps at c=16 |
| what saturates first | the host's 2-core quota | the guest SQL pool, past c=16 |
| overhead ratio, steady | 6.71 (one sample) | **11.94 median** (9.34–13.61), red against 9 |

**The request path is single-digit milliseconds on the release host: 4.4 ms in the
host, 7.0–7.8 ms at an in-cluster client.** Per-request host CPU fell 5.6-fold, the
knee moved from four clients to eight, and peak throughput rose tenfold. The direct
statement did not move, as it should not have.

## Where 4.4 ms goes now

`tools/phases.py` over the five steady traces, the same decomposition as
`5-residue-spans.md`:

| ms per request | avg of 5 | share | was (6-throughput, debug) |
|---|---:|---:|---:|
| fork host calls (route, lookups, new_store, incoming request, flow guest instantiate, store drop) | 0.30 | 6 % | 1.24 |
| fork glue | 0.15 | 3 % | 0.69 |
| flow-http guest's own path | 0.65 | 14 % | 2.77 |
| route plugin spans | 0.13 | 3 % | 0.46 |
| **authenticate** (pat 0.90, roles 0.31, permissions 0.36) | **1.66** | **35 %** | 2.07 |
| `component.invoke`, named (statement 0.28, session settings 0.28, instantiate 0.11, link 0.10, …) | 1.23 | 26 % | 3.73 |
| `component.invoke`, its own (driver + data-access guest) | 0.67 | 14 % | 1.80 |
| total (`handle_http_request`) | 4.80 | | 12.76 |

Everything platform-owned collapsed with the profile except the three authentication
round trips, which are server round trips and now lead: `wamn.auth.pat` alone is
0.90 ms, the largest single span in the request. Instantiate is 0.11 ms (from 1.25),
`linker_setup` 0.10 (from 0.72). The unnamed gaps total 1.5 ms, 35 % — the same
share as on the debug host, in the same places (driver before `cache_hit` 0.28, the
data-access guest's start and return 0.19 + 0.24, the flow guest's start and response
0.15 + 0.15). `wamn-0h0g.17.28` scopes those; they are half a millisecond apiece now.

## The knee, and what saturates first now

| layer | knee | p99 turns at | peak | at c |
|---|---:|---:|---:|---:|
| `route` | **8** (1,543 req/s) | 16 (11.7 → 17.2 ms) | 1,652 req/s | 16 |
| `nodb` | **8** (10,828 req/s) | 16 (2.3 → 3.3 ms) | 12,897 req/s | 32 |
| `pg` | **8** (73,259 tps) | 16 (0.31 → 0.72 ms) | 78,839 tps | 16 |

**Past sixteen clients the route's throughput falls** — 1,378 req/s at c=32, 823 at c=64
with p50 74 ms and p99 135 ms — while the host's CPU *falls* with it (3.1 → 1.9 cores,
never throttled), PostgreSQL stays at 0.3–0.6 cores, and the project database's
backends sit at 15–16, the pool caps (`WAMN_PG_GUEST_POOL_MAX` 14 plus the callable-http
pool). Requests past the pool wait for a connection and hold it across the whole
invoke; the server is not the limit, the acquisition queue is (`wamn-0h0g.17.32`,
recorded, not asserted). The no-DB layer, with no pool in its path, keeps scaling to
c=32 on 4.0 cores and only bends at c=64 where the generator and the host share the
box. The statement layer is the machine's, as before.

**Host CPU per request** is 5.6 ms single-stream and 2.6 ms from c=8 on; between
steps the host burns 0.04 cores. The non-proportional part is now about half a
core under load, down from a core — smaller, still there, unmeasured
(`wamn-0h0g.17.31`, second half).

## The ratio gate: red, for a new reason

The five steady ratios read 13.61, 12.52, **11.94**, 10.20, 9.34 — median 11.94
against the ceiling 9 — on a host whose request just fell from 12.8 to 4.4 ms. The
ratio is `handle_http_request / (statement + instantiate)`, and its denominator is
what the release profile removed: instantiate went from 1.25 to 0.11 ms, the statement
stayed at 0.28 (server-bound), so the denominator fell 4× while the numerator fell
2.7×. The gate is not measuring overhead here; it is measuring how much of the
request is PostgreSQL. This report leaves the ceiling at 9 and the run red, and puts
the number to the owner on `wamn-0h0g.17.29`: re-baseline at 12 on the release
profile (the measured median, ratcheting from there), or redefine the denominator.
The median gate itself did its job — one sample read 9.34 and would have passed.

## Per layer

### `route` — oha against `POST /purchase_order/get`, authenticated

| c | req/s | p50 ms | p99 ms | errors | cut off | answered | server commits/s | backends | host cores (window) | host CPU ms/req | throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 263 | 3.39 | 9.78 | 0 | 1 | 2625 | 638 | postgres=2 project=4 | 0.81 | 5.6 | 0 % | 0.09 |
| 4 | 1089 | 3.39 | 7.84 | 0 | 4 | 10889 | 3704 | postgres=2 project=8 | 2.38 | 3.0 | 0 % | 0.38 |
| 8 | 1543 | 4.83 | 11.72 | 0 | 8 | 15429 | 5549 | postgres=2 project=12 | 2.95 | 2.6 | 0 % | 0.52 |
| 16 | 1652 | 9.31 | 17.15 | 0 | 16 | 16511 | 5900 | postgres=2 project=12 | 3.09 | 2.6 | 0 % | 0.56 |
| 32 | 1378 | 22.60 | 40.64 | 0 | 32 | 13754 | 5152 | postgres=2 project=15 | 2.71 | 2.8 | 0 % | 0.46 |
| 64 | 823 | 74.29 | 134.74 | 0 | 64 | 8168 | 2969 | postgres=2 project=16 | 1.93 | 3.2 | 0 % | 0.33 |

### `nodb` — oha against `GET /no-such-route`, answered 404 by the guest

| c | req/s | p50 ms | p99 ms | errors | cut off | answered | host cores (window) | host CPU ms/req | throttled |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 1934 | 0.48 | 1.18 | 0 | 1 | 19347 | 1.14 | 0.8 | 0 % |
| 4 | 5300 | 0.59 | 2.78 | 0 | 4 | 53009 | 2.16 | 0.6 | 0 % |
| 8 | 10828 | 0.65 | 2.25 | 0 | 8 | 108305 | 3.59 | 0.5 | 0 % |
| 16 | 12020 | 1.27 | 3.28 | 0 | 16 | 120208 | 3.79 | 0.4 | 0 % |
| 32 | 12897 | 2.43 | 5.23 | 0 | 32 | 128967 | 3.96 | 0.4 | 0 % |
| 64 | 9577 | 5.88 | 19.77 | 0 | 64 | 95808 | 3.13 | 0.5 | 0 % |

### `pg` — pgbench, the generated `purchase_order/get` read, direct

| c | tps | p50 ms | p99 ms | failed | transactions | server commits/s (window) | pg cores (window) |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 18,326 | 0.05 | 0.13 | 0 | 183,143 | 8,420 | 0.32 |
| 4 | 44,923 | 0.08 | 0.18 | 0 | 448,615 | 32,528 | 1.85 |
| 8 | 73,259 | 0.08 | 0.31 | 0 | 730,989 | 53,192 | 3.03 |
| 16 | 78,839 | 0.17 | 0.72 | 0 | 785,570 | 56,784 | 3.28 |
| 32 | 77,051 | 0.33 | 1.75 | 0 | 761,434 | 54,577 | 3.26 |
| 64 | 61,234 | 0.58 | 6.97 | 0 | 598,188 | 43,191 | 2.92 |

Other receipts: cold 104.7 ms, restart-first 123.6 ms with 75 s of recovery on the
previous run; this run's are in `journey/first-request-*.receipt`. The overhead-ratio
gate failed after the sweep, so the failure capture ran and the cluster was deleted
with a passing cleanup receipt; every sweep artifact was already on disk.

## Method

As `6-throughput.md`: `tools/receiving-cluster-journey-run --apply --throughput` from
the lane; oha 1.16.0 and pgbench 18.6 in pods pinned by digest; `wamn-throughput
sample` and `report`; cores, CPU per request and the throttled share from the host
pod's and the PostgreSQL container's `cpu.stat` across each sample window; pgbench's
log sampled at 5 % this run. Shape gate `throughput_bench_live` over
`journey/throughput/`. Decomposition of the steady traces by
`tools/phases.py` and `tools/gaps.py`.

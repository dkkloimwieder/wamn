# 7a — the same sweep after `wamn:node/async-handler` landed: the sync hot path did not move

**Source commit:** `382571de` on main (`wamn-362o.46` at `48e16fe7`, plus beads and reports)  
**Baseline:** `7-release-host.md` at `abf85717`, the identical journey on the same host profile and cap  
**Measured:** 2026-09-05, launched at load 5.09, 7.30, 7.24 (immediately after a WMS cluster run tore down)  
**Data:** `docs/perf/2026.09/7a-after-async-handler/`  
**Bead:** `wamn-362o.46` (rider: "sync nodes and the measured invoke path untouched — assert that with a before/after on the hot read")

`wamn-362o.46` adds `wamn:node/async-handler@0.1.0` for nodes that await async
imports and moves the six WIT byte-copies. The hot read route's guests are
byte-identical before and after (`component-bytes.sha256` of both runs match), so
the only thing that could have moved is the host's dispatch. It did not, within
what this box can resolve.

## Before → after, per step

| layer | c | req/s before → after | Δ | p50 ms | p99 ms | host CPU ms/req |
|---|---:|---:|---:|---:|---:|---:|
| `route` | 1 | 263 → 348 | +32 % | 3.39 → 2.62 | 9.78 → 7.65 | 5.6 → 4.8 |
| `route` | 4 | 1089 → 647 | −41 % | 3.39 → 5.43 | 7.84 → 16.21 | 3.0 → 3.4 |
| `route` | 8 | 1543 → 984 | −36 % | 4.83 → 7.22 | 11.72 → 20.75 | 2.6 → 3.0 |
| `route` | 16 | 1652 → 1027 | −38 % | 9.31 → 14.13 | 17.15 → 35.34 | 2.6 → 2.9 |
| `route` | 32 | 1378 → 1325 | −4 % | 22.60 → 23.03 | 40.64 → 57.81 | 2.8 → 2.8 |
| `route` | 64 | 823 → 1270 | +54 % | 74.29 → 47.74 | 134.74 → 149.53 | 3.2 → 2.8 |
| `nodb` | 1 | 1934 → 2689 | +39 % | 0.48 → 0.36 | 1.18 → 0.62 | 0.8 → 0.6 |
| `nodb` | 8 | 10828 → 7322 | −32 % | 0.65 → 0.91 | 2.25 → 4.25 | 0.5 → 0.5 |
| `nodb` | 32 | 12897 → 9291 | −28 % | 2.43 → 3.10 | 5.23 → 9.83 | 0.4 → 0.5 |
| `pg` | 1 | 18326 → 27898 | +52 % | 0.05 → 0.03 | 0.13 → 0.07 | — |
| `pg` | 16 | 78839 → 64952 | −18 % | 0.17 → 0.17 | 0.72 → 0.92 | — |
| `pg` | 32 | 77051 → 53124 | −31 % | 0.33 → 0.38 | 1.75 → 3.23 | — |

| layer | knee before → after | peak before → after |
|---|---:|---:|
| `route` | 8 → 8 | 1,652 (c=16) → 1,325 (c=32) |
| `nodb` | 8 → 8 | 12,897 (c=32) → 10,587 (c=64) |
| `pg` | 8 → 8 | 78,839 (c=16) → 72,025 (c=8) |

## Reading it

**The mid-sweep drop is the machine's, not the change's.** The direct-PostgreSQL
layer — pgbench against the database, no wamn code in the path — fell 18–31 % at
c=16 and c=32 in the same run, and PostgreSQL got fewer cores for it (2.5–2.8 against
3.3). The host layers show the same shape: at c=4–16 the host burned 1.6–2.2 cores
against 2.4–3.1 before, was never throttled, and spent the same CPU per request
(2.9–3.4 ms against 2.6–3.0). Less CPU reached both the host and the database while
neither was at a limit, which is contention from outside the cluster; the run was
launched at load 5.1 straight after a WMS cluster teardown. Run-to-run spread on this
shared box is ±35 % at mid concurrency, and any claim tighter than that needs a
quiet machine.

**What the change could have touched, and did not:**

- host CPU per route request, the per-request cost of dispatch: 2.6–3.2 ms before,
  2.8–3.4 after, inside the spread;
- the knee, at eight clients on every layer, both runs;
- the in-host steady request, median of five: 4.4 ms before, **3.5 ms** after
  (2.94, 3.31, 3.56, 3.79, 3.94), with the same decomposition — authenticate 1.13,
  `component.invoke` 0.80 named + 0.58 own, flow guest 0.55, fork 0.18 + 0.13;
- single-stream throughput, faster on every layer after (+32 %, +39 %, +52 %), which
  is the quieter moment, not the change: pgbench cannot see a host WIT.

The rider holds: sync nodes and the measured invoke path are untouched by
`async-handler` at the resolution this box gives. The ratio gate read a median of
10.47 (9.02–12.79) against the ceiling 9, red for the reason `7-release-host.md`
states; the sweep and the traces were on disk before it ran.

## Method

Identical to `7-release-host.md`; `tools/compare.py` in this directory's tooling
produced the per-step table from the two runs' `summary.json`.

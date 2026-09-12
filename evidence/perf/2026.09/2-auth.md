# 2 — collapse the authentication round trips

Fix 2. The owner ruled collapse over cache: it removes the same work with no
revocation window and no invalidation surface. The cache stays filed as a
trigger, only if the remainder ever matters and rotation invalidation is
designed first.

| | |
|---|---|
| source commit | `1fd9b222` (`perf/2-auth`) |
| baseline | `2a-auth-instrument.md` at `8669ba27` |
| load average at launch | 14.33 13.93 9.13 |
| load average at finish | 2.17 6.39 7.69 |
| route | `POST /purchase_order/get`, `Host: receiving.localhost` |
| samples | one cold, four hot, one write |

## What changed

1. **`BEGIN` and `COMMIT` are gone from `operation_permissions`.** The read
   installs no session claim, so it is the shape 3c proved needs no transaction.
2. **`statement_timeout` moved to a `post_create` pool hook** at session scope,
   applied once per connection instead of once per request.
3. **The three statements are prepared once.** `PreparedIdentityReads` parses the
   two identity statements at construction; the pooled permission read uses
   `prepare_cached`.

## Result

Hot means over four samples, against `2a`:

| | 2a (ms) | 2 (ms) | change |
|---|---:|---:|---:|
| `auth.pat` | 1.184 | 0.665 | **−44 %** |
| `auth.roles` | 0.709 | 0.415 | **−41 %** |
| `auth.permissions` | 2.164 | 0.684 | **−68 %** |
| ↳ `perm.begin` | 0.318 | — | gone |
| ↳ `perm.timeout` | 0.293 | — | gone |
| ↳ `perm.query` | 0.740 | 0.371 | **−50 %** |
| ↳ `perm.commit` | 0.344 | — | gone |
| leg residue | 0.246 | 0.231 | −6 % |
| **`route.authenticate`** | **4.303** | **1.994** | **−53.7 %** |

**Nine server round trips became three.** One per identity read and one for the
permission read; the first request on each newly created pooled connection still
pays a single `Parse` for the cached statement.

**The projection was confirmed by measurement.** `2a` predicted that removing the
`Parse` round trip would roughly halve each read, from the 0.30 ms this cluster
charges per round trip. `perm.query` went 0.740 → 0.371, exactly halved, on a
statement whose text did not change. That is the cleanest available proof that
the extra round trip was real and is now gone.

## Per-trace

| trace | auth.pat | auth.roles | auth.permissions | ↳acquire | ↳begin | ↳timeout | ↳query | ↳commit | residue | **authenticate** |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| cold | 0.646 | 0.314 | 0.744 | 0.15 | 0 | 0 | 0.426 | 0 | 0.204 | **1.908** |
| hot-1 | 0.527 | 0.279 | 0.54 | 0.1 | 0 | 0 | 0.306 | 0 | 0.153 | **1.499** |
| hot-2 | 0.808 | 0.638 | 0.772 | 0.145 | 0 | 0 | 0.424 | 0 | 0.297 | **2.515** |
| hot-3 | 0.654 | 0.349 | 0.704 | 0.15 | 0 | 0 | 0.369 | 0 | 0.252 | **1.959** |
| hot-4 | 0.669 | 0.393 | 0.719 | 0.133 | 0 | 0 | 0.384 | 0 | 0.22 | **2.001** |
| write | 0.504 | 0.319 | 0.682 | 0.141 | 0 | 0 | 0.354 | 0 | 0.211 | **1.716** |

Whole request:

| trace | auth | linker_setup | link | instantiate | postgres | statement | residue | **total** | ratio |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| cold | 1.908 | 2.557 | 0.124 | 0.976 | 1.396 | 0.592 | 6.698 | **13.97** | 8.91 |
| hot-1 | 1.499 | 3.081 | 0.173 | 1.268 | 1.147 | 0.45 | 6.411 | **13.83** | 8.05 |
| hot-2 | 2.515 | 2.944 | 0.153 | 1.55 | 1.915 | 0.868 | 8.275 | **17.734** | 7.34 |
| hot-3 | 1.959 | 3.75 | 0.225 | 1.322 | 1.47 | 0.64 | 8.046 | **17.083** | 8.71 |
| hot-4 | 2.001 | 2.894 | 0.15 | 1.236 | 1.412 | 0.562 | 7.713 | **15.724** | 8.75 |
| write | 1.716 | 3.868 | 0.201 | 1.679 | 7.507 | 3.207 | 8.187 | **23.747** | 4.86 |

Hot overhead ratio: **8.21 mean**, from 9.07. The gate ceiling is 12.

## A correction this work forced

`wamn-0h0g.17.18` said the pool-uniform session settings — `statement_timeout`,
`search_path` and `app.runner` — all belong in connection setup. **Only
`statement_timeout` does.** It comes from `ResolvedCredential`, resolved per
project, class and tenant, which is exactly the pool key. `search_path` and
`app.runner` come from `schema_for` and `runner_for`, which are keyed by
**component id**, while the guest pool is keyed by class, project and tenant. Two
components of one tenant share a connection and can want different values, so
hoisting those would leak one component's `search_path` to the next borrower.
The bead is narrowed, not closed, and the guest autocommit path still pays its
one settings round trip.

## Load

This run launched at a one-minute load average of **14.33**, against **2.57** for
the `2a` baseline it is compared with — the workspace `--all-targets` sweep was
still finishing. That cuts one way only: the auth legs got faster by half while
the machine was five times busier, so the collapse is not a load artefact. It
does mean the whole-request totals and the write sample are worse than a quiet
machine would show, and neither is claimed as a result.

## What is not a result

**Cold is not comparable to `2a`.** In `2a` the probe won the race against the
journey's own first request and paid both pool connects — 45.5 ms and 51.9 ms —
for a 121 ms cold. Here the journey's request went first, so the probe found warm
pools. The comparable pair is the journey's own cold receipt: **24.4 ms → 16.5
ms**, and even that moved with the collapse and the machine together.

**The write sample is slower than `2a`** (23.7 ms against 20.2 ms) on a
five-minute load average of 6.39. Its `postgres` span alone is 7.5 ms against
4.8 ms. Nothing in this change touches the write path; this is load.

## Gate status

Nothing asserts the overhead ratio yet **in practice**. The assertion is armed at
12 in `collect_trace_breakdown`, but the journey never reaches it: the
`--measure-startup` restart arm still fails, so `steady` is never collected.
The ratio gate is dead code until the restart arm is fixed. See
`2-auth/restart-watch.log` and `wamn-0h0g.17.20`.

## Raw data

`2-auth/` holds the six traces, `auth-table.md`, `phase-table.md`,
`span-trees.txt`, `restart-watch.log`, the client log, the launch load average
and the journey evidence. `tools/auth-row.sh`, `tools/span-tree.sh`,
`tools/phase-row.sh` and `tools/measure.sh` regenerate every number.

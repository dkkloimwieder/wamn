# 1b-a — split `linker_setup`

Measurement only. This run exists so 1b is scoped against numbers rather than
against the plan that was banked for it.

| | |
|---|---|
| source commit | `e225143e` (`perf/1b-linker`) |
| load at launch | 8.43 4.12 2.58 |
| samples | one cold, four hot, one write |

After the auth collapse, `wamn.component.linker_setup` was the largest named
phase left in the request — larger than the whole permission read. Four spans
split it: the WASI p2 surface added to a fresh `Linker`, the nested operation
links, the plugin binds, and `Store` construction.

## Result, hot means over four samples

| span | ms | share |
|---|---:|---:|
| `wamn.linker.wasi` | 1.781 | **55 %** |
| residue — per-request host construction | 1.053 | 32 % |
| `wamn.linker.plugins` | 0.305 | 9 % |
| `wamn.linker.store` | 0.093 | 3 % |
| `wamn.linker.nested` | **0.033** | **1 %** |
| **`linker_setup`** | **3.264** | |

## Per-trace

| trace | linker_setup | ↳wasi | ↳nested | ↳plugins | ↳store | ↳residue |
|---|---:|---:|---:|---:|---:|---:|
| cold | **2.521** | 1.233 | 0.023 | 0.293 | 0.092 | 0.88 |
| hot-1 | **2.386** | 1.175 | 0.023 | 0.273 | 0.08 | 0.834 |
| hot-2 | **3.814** | 2.178 | 0.038 | 0.324 | 0.104 | 1.17 |
| hot-3 | **3.987** | 2.285 | 0.046 | 0.343 | 0.096 | 1.218 |
| hot-4 | **2.869** | 1.487 | 0.023 | 0.28 | 0.091 | 0.988 |
| write | **2.684** | 1.4 | 0.023 | 0.278 | 0.084 | 0.901 |

## The finding

**The banked plan was aimed at 1 %.** 1b was scoped as *move nested-invocation
state into `SharedCtx`, then cache the `Linker`* — and binding the nested links
costs 0.033 ms. The refactor that plan required buys nothing measurable.

The cost is the WASI p2 surface, and nothing in it is per-request: identical host
functions against the identical engine, rebuilt from scratch on every call. That
is a one-field change, not a refactor. `1b-linker-clone.md` carries it and the
result.

This is the second time in this program that instrumenting first turned an
invasive change into a small one — 3c did it to the transaction ceremony.

## Honesty

This run launched at a one-minute load average of 8.43, so its absolute totals
are not comparable with quieter runs. The shares above are quotients within each
trace and do not move with the machine, which is what the conclusion rests on.

## Raw data

`1b-a-linker-instrument/` holds the six traces, `linker-table.md`,
`span-trees.txt`, the journey evidence and the launch load average.

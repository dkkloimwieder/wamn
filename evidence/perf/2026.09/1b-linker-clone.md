# 1b — link the WASI surface once, clone it per request

| | |
|---|---|
| source commit | `c64419a6` (`perf/1b-linker`) |
| baseline | `1b-a-linker-instrument.md`, the same tree with the rebuild in place |
| load at launch | 1.40 1.76 2.07 (baseline run: 8.43) |
| samples | one cold, four hot, one write |

## The measurement that re-scoped the fix

`1b-a` split `linker_setup` (3.264 ms hot) before anything was changed:

| span | ms | share |
|---|---:|---:|
| `wamn.linker.wasi` | 1.781 | **55 %** |
| residue — per-request host construction | 1.053 | 32 % |
| `wamn.linker.plugins` | 0.305 | 9 % |
| `wamn.linker.store` | 0.093 | 3 % |
| `wamn.linker.nested` | **0.033** | **1 %** |

The banked plan for 1b was *move nested-invocation state into `SharedCtx`, then
cache the `Linker`*. **Binding the nested links costs 0.033 ms.** That refactor —
the invasive part — buys one percent. The cost is the WASI p2 surface, and
nothing in it is per-request: identical host functions against the identical
engine, rebuilt from scratch every time.

## The change

The driver links WASI once at construction and each request clones. `Linker<T>`
is `Clone` at wasmtime 47.0.1; the clone copies a name map, while
`add_to_linker_async` builds hundreds of closures and registers their types.

No isolation changes. Every request still gets its own linker and layers its own
binds — nested, the plugin hooks, connection-http, blobstore — on the clone. It
just stops rebuilding the shared half. Nested invocations clone the same base.

## Result

| | rebuild | clone | |
|---|---:|---:|---:|
| the WASI span | 1.781 | **0.593** | **−67 %** |
| `wamn.linker.plugins` | 0.305 | 0.350 | — |
| `wamn.linker.nested` | 0.033 | 0.026 | — |
| `wamn.linker.store` | 0.093 | 0.089 | — |
| residue | 1.053 | 0.929 | — |
| **`linker_setup`** | **3.264** | **1.987** | **−39 %** |

`linker_setup`'s share of the request fell from **21.1 % to 15.1 %** — a quotient
within each trace, so it does not move with the machine, and it agrees with the
span.

## Per-trace

| trace | linker_setup | ↳clone | ↳nested | ↳plugins | ↳store | ↳residue | auth | instantiate | postgres | total | ratio |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| cold | **2.456** | 0.602 | 0.023 | 0.439 | 0.111 | 1.281 | 1.694 | 1.631 | 1.481 | 14.779 | 6.739 |
| hot-1 | **1.949** | 0.564 | 0.022 | 0.386 | 0.086 | 0.892 | 2.189 | 1.226 | 1.648 | 14.361 | 7.157 |
| hot-2 | **2.053** | 0.558 | 0.026 | 0.379 | 0.1 | 0.989 | 1.906 | 1.238 | 1.192 | 12.884 | 7.475 |
| hot-3 | **3.265** | 0.781 | 0.034 | 1.199 | 0.112 | 1.14 | 1.535 | 1.068 | 1.287 | 13.67 | 8.064 |
| hot-4 | **2.107** | 0.576 | 0.023 | 0.37 | 0.089 | 1.049 | 3.172 | 1.668 | 1.925 | 18.24 | 7.261 |
| write | **2.394** | 0.582 | 0.025 | 0.359 | 0.114 | 1.315 | 2.254 | 1.341 | 14.19 | 28.359 | 7.39 |

## The journey passed end to end, for the first time

```
RECEIVING STARTUP MEASUREMENT PASS source=c64419a68af8 namespace=wamn-receiving-journey
verdict=pass phase=restart-first status=200 recovery_seconds=76 ceiling_seconds=120
verdict=pass phase=steady overhead_ratio=8.659 ceiling=12
```

**The overhead-ratio gate has now executed.** It had never run: the restart arm
failed before it for seven consecutive journeys. Getting there took four
corrections, in the order the chain revealed them.

| link | cause |
|---|---|
| the probe gave up at 45 s against an 89 s recovery | predates fix 1 |
| `trace_is_complete` required `pull == 1` and `compile == 1` | fix 1 moved both to startup |
| `restart-first` expected an `executor-platform` acquire | same |
| `total_ms` read with a sed anchored on end-of-line | **introduced here**, by appending `recovery_seconds` after it |
| `local a=$1 b=…$a…` under `set -u` | **introduced here** |

The third of those is worth keeping: the replacement for the `pull`/`compile`
pair — `cache_hit == 1` with `pull == 0` and `compile == 0` — is strictly
stronger. It fails if the preload stops working AND if a served request starts
pulling again, where the old pair only checked that a pull happened at all,
which after fix 1 is a request that has gone wrong.

`restart-first`'s own ratio is **37.6**, and the gate deliberately does not
assert it: a restarted host pays two 38 ms pool connects, which is reconnection
rather than platform overhead.

## What is left inside linker_setup

The residue is now the largest part of it: 0.93 ms of `ConnectionHttp::new`,
`WamnBlobstore::new` and `NestedOperationHost{}` built per request. The first two
take only driver-level arguments and look hoistable — but they are scope-keyed
registries relying on `revoke_invocation(&scope)` for isolation, so hoisting them
moves an isolation boundary and needs its own measurement and its own argument.

## Raw data

`1b-linker-clone/` holds the six traces, `linker-table.md`, `span-trees.txt`, the
journey evidence including the three passing receipts, and the launch load.
`1b-a-linker-instrument/` holds the rebuild baseline it is compared against.

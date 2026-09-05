# 1c — what `linker_setup` is actually made of

Measurement only, in two passes. 1b left 1.02 ms of `linker_setup` unnamed; this
names it, and then names the largest thing inside what it found.

| | |
|---|---|
| source commits | `1c-a` residue split, `1c` scope split |
| load at launch | 3.57 (residue), 3.89 (scope) |

## Pass one: the residue

Hot means over four samples.

| span | ms | share |
|---|---:|---:|
| `wamn.linker.clone` | 0.777 | 29 % |
| **`wamn.linker.scope`** | **0.766** | **28 %** |
| `wamn.linker.plugins` | 0.419 | 16 % |
| still unnamed | 0.410 | 15 % |
| `wamn.linker.imports` | 0.243 | 9 % |
| `wamn.linker.hosts` | **0.035** | 1 % |
| `wamn.linker.workload` | 0.032 | 1 % |
| `wamn.linker.nested` | 0.018 | 1 % |
| **`linker_setup`** | **2.700** | |

**The three per-request host constructions cost 0.035 ms.** `ConnectionHttp::new`,
`WamnBlobstore::new` and `NestedOperationHost{}` were the assumed residue, and
hoisting them — which would have moved an isolation boundary — buys nothing.

`wamn.linker.scope` had never been named and is the second largest item.

## Pass two: inside `scope`

| span | ms | share |
|---|---:|---:|
| **`wamn.linker.pending_scope`** | **0.605** | **68 %** |
| `wamn.linker.ctx` | 0.117 | 13 % |
| gap | 0.116 | 13 % |
| `wamn.linker.revokes` | **0.051** | 6 % |
| **`scope`** | **0.889** | |

`PendingStatementScope::bind` alone. The four claim revokes are 0.051 ms — the
microseconds four map operations should cost — so "the revokes are slow because
they are security-relevant" was never true, and the number says where to look.

## Three for three

Every design-named target this week measured near zero, and the cost was
somewhere nobody had named:

| fix | the plan's target | measured | the real cost |
|---|---|---:|---|
| 3c | the transaction model | — | a per-statement boolean from PostgreSQL |
| 1b | nested-invocation binding | 0.033 ms | a WASI linker rebuilt per request |
| 1c | three host constructions | 0.035 ms | `PendingStatementScope::bind` |

**Scope the refactor after the span, not before.**

## What this leaves for `InstancePre`

Cacheable per digest: `clone`, the linker half of `plugins`, `imports`,
`workload`, `nested`. Not cacheable: `scope`, which is per-request claim
revocation — and whose cost turns out to be one call inside it, not the
revocation.

The blocker is `on_workload_item_bind` doing two unrelated jobs in one hook:
scope-keyed registration and linker population. The linker half already passes
`extract_active_ctx`, so it is store-native and per-digest-safe today. Splitting
the two is the next change.

## Rule

Absolute milliseconds are not comparable across runs — the journey builds on the
machine it measures, and launch load across this series ran 1.26 to 14.33. Shares
within a trace carry every conclusion, and every report records its launch load.

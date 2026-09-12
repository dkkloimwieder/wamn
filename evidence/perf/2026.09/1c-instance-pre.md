# 1c — one `InstancePre` per digest

The linker is built once per digest and sealed into an `InstancePre`; a request
instantiates from it. The blocker named in `1c-residue-and-scope.md` --
`on_workload_item_bind` doing two jobs in one hook -- is un-fused first.

| | |
|---|---|
| source commit | `49974184` as measured (the receipts say so); rebased before landing |
| bead | `wamn-0h0g.17.15` |
| load at launch | **11.10** -- another agent's build shared the machine; read the shares, not the milliseconds |

## The split

`WamnPostgres` and `WamnLogging` each gain two inherent methods:

- `register_workload_scope(scope, config, interfaces)` -- the per-REQUEST half:
  every claim keyed by the scope. Counted.
- `add_linker_entries(linker, [component,] interfaces)` -- the per-DIGEST half:
  host definitions on the linker, capturing nothing per request. Counted.

The trait hook calls both, in that order, for the wash host path. The driver
calls them separately: entries when a digest is first prepared, registration on
every request.

**The order-and-count test** drives the production `instantiate_compiled` three
times -- twice to one digest, once to another -- and asserts the four counters
read `(1,1,1,1)`, `(1,2,1,2)`, `(2,3,2,3)`, then that each digest's entries span
ends before its first registration starts and that the hit carries no entries
span at all. Two mutants killed it for the intended reason before it was
trusted: a cache that never hits (the silent failure, caught at request two with
`(2,2,2,2)`) and a skipped postgres registration (the loud one, caught at request
one with `(1,0,1,1)`).

## What moved where

| per digest, in `wamn.component.link` on a miss | per request, in `wamn.component.linker_setup` |
|---|---|
| base-linker clone | the three host objects |
| nested-operation links | scope mint |
| `WorkloadComponent` + import projection | plugin registration |
| plugin linker entries | four revokes |
| `instantiate_pre` | `PendingStatementScope::bind` |
| | plugin map + `Ctx` + `Store::new` |

Two things the cache is careful about. The nested links come from the ADMITTED
FACTS, not the bytes, so the entry stores the link map and a hit is compared
against the request's facts before it is used -- a digest readmitted under
different dependency pins rebuilds instead of reusing. And the candidate-bytes
path never caches: nothing there verified that the digest names those bytes.

`wamn.component.link` keeps its name and its once-per-request guarantee (the
journey trace guard requires exactly one), but on a hit it now measures a map
lookup and a clone, and carries `wamn.prepared_hit=true`.

## Before

Hot means over four samples, `1c-scope-split/` traces, load 3.89.

| span | ms |
|---|---:|
| `wamn.component.linker_setup` | 2.779 |
| `wamn.component.link` | 0.197 |
| `wamn.component.instantiate` | 1.534 |

## After

Hot means over four samples, `1c-instance-pre/` traces, load 11.10. Every
request in the run -- the cold arm's included -- hit the cache: the readiness
preload had already prepared both digests, in 5 ms each (`journey/host-cold.log`,
`release component instantiation completed elapsed_ms=5`), so the miss path
never appears in a request trace at all.

| span | before (load 3.89) | after (load 11.10) | |
|---|---:|---:|---|
| `wamn.component.linker_setup` | 2.779 | **1.784** | the per-request half only |
| `wamn.component.link` | 0.197 | **0.090** | lookup + clone, `wamn.prepared_hit=true` ×4 |
| `wamn.component.instantiate` | 1.534 | 1.826 | unchanged work, three times the load |
| `wamn.linker.clone` | 0.777 | -- | gone from the request |
| `wamn.linker.plugins` | 0.419 | -- | gone |
| `wamn.linker.imports` | 0.243 | -- | gone |
| `wamn.linker.workload` | 0.032 | -- | gone |
| `wamn.linker.nested` | 0.018 | -- | gone |
| `wamn.linker.register` | -- | 0.147 | new: the registration half, per request |
| `wamn.linker.scope` | 0.889 | 1.330 | per request, still; grew with the load |
| `wamn.linker.pending_scope` | 0.605 | 0.969 | 73 % of `scope` |

**The five per-digest spans left the request path**: 1.49 ms of work at load 3.89
became 0.09 ms of lookup at load 11.10. `linker_setup` dropped a full millisecond
against a machine three times as busy; under matched load the drop is larger,
and `instantiate` growing 19 % says how much the load cost everything else.

`wamn.linker.scope` is now 75 % of `linker_setup` and `PendingStatementScope::bind`
is 73 % of that. It is the next thing, and it is per request by design.

## Gate

| | |
|---|---|
| overhead ratio | **6.49**, ceiling 12 (previous runs 8.22, 8.66, 9.08) |
| recovery after restart | 37 s, ceiling 120 s |
| journey | pass, all three arms |

The ratio is load-tolerant, not load-invariant: its denominator carries
`instantiate`, which grew under this load, so part of the move from ~8.7 to 6.49
is the load and not the fix. The ceiling is not ratcheted in this commit; the
number to set it to is the owner's call and is raised in the report-out.

## What this leaves

| per request, ms | |
|---|---:|
| `wamn.linker.pending_scope` | 0.969 |
| `wamn.linker.ctx` | 0.165 |
| `wamn.linker.register` | 0.147 |
| `wamn.linker.revokes` | 0.050 |
| `wamn.linker.hosts` | 0.030 |
| unnamed inside `linker_setup` | 0.42 |

`PendingStatementScope::bind` rebinds every operation's verified statement set
into the plugin on every request. The statement sets are per digest; the scope
they bind under is per request. That is the same shape this change just
un-fused, one layer down.

## What the driver's registration half is

The driver passes an empty workload config -- identity is installed by
`bind_acquisition`, not by the bind -- so under the driver the postgres half
registers nothing and warns once per instantiation (`component imports
wamn:postgres but sets no wamn.tenant; calls will be refused`), and the logging
half is gated out entirely: no served guest imports `wasi:logging`. That is
exactly what the fused hook did on every instantiation before this change; the
split preserves it and the count test pins it. The warn is pre-existing and
filed as `wamn-0h0g.17.24`, not fixed here.

# 1c-b — inside `pending_scope`

Measurement only. After the `InstancePre`, `PendingStatementScope::bind` was
0.969 of the 1.330 ms `scope` block and nothing inside it had a name. This
names it before anyone touches it.

| | |
|---|---|
| source commit | `fb1b5a99` as measured |
| bead | `wamn-0h0g.17.25` |
| load at launch | 2.04, rising to 6.8 by the end (another agent's build resumed mid-run) |

## What runs per request

For every operation of the component: lower each verified statement out of the
admitted facts (a clone of the SQL text, the binds and the columns), then bind
the set under the request's scope -- which SHA-256s every statement's SQL
against its digest, Arc-wraps each statement, and inserts under a write lock.
The clear that precedes it drops the previous binding for the scope.

Split as: `wamn.scope.clear`, `wamn.scope.lower` (all operations),
`wamn.scope.bind` (all operations), and `wamn.scope.verify` inside each bind,
one span per operation, summed per request.

## Result

Hot means over four samples, per request, `1c-b-scope-split/` traces. The
component exports eight operations, so `verify` is eight spans summed.

| span | ms / request | share of `pending_scope` |
|---|---:|---:|
| `wamn.linker.pending_scope` | 1.644 | |
| `wamn.scope.bind` | 1.395 | 85 % |
| **`wamn.scope.verify`** (8 per request) | **0.845** | **51 %** |
| `bind` minus `verify` (Arc-wrap, write lock, insert, span overhead) | 0.550 | 33 % |
| `wamn.scope.lower` | 0.086 | 5 % |
| `wamn.scope.clear` | 0.028 | 2 % |
| gap | 0.135 | 8 % |

The cold request reads the same shape: `verify` 0.689 of `bind` 1.161 of
`pending_scope` 1.379.

| gate | |
|---|---|
| overhead ratio | 8.25, ceiling 12 (6.49 last run) |
| recovery after restart | 74 s of 120 |
| journey | pass, all three arms |

The ratio rose from 6.49 because `instantiate` in its denominator read 1.212 ms
this run against 1.815 under the last run's load, and because this build adds
eight spans to every request's numerator. Load-tolerant, not load-invariant, as
recorded; the gate is the same 12 in both runs and passed both.

## Reading

**The SHA-256 is the cost.** Every request hashes every verified statement's
SQL text against its digest -- the same text, the same digest, the same answer
-- for all eight operations. Lowering the statements out of the admitted facts,
the assumed suspect, is 5 %.

Every input to `verify` and `lower` is per DIGEST: the admitted facts are
immutable and already named the component. Only the scope the bound set is
inserted under is per request. That is the fusion the `InstancePre` un-fused,
one layer down, and the cut is the same shape: verify and lower once when the
digest is prepared, hold one `Arc<BoundStatementSet>` per operation beside the
`PreparedComponent`, and let the per-request bind insert the Arcs under the
scope. The digest-mismatch refusal moves to preparation, where it fires once
and refuses the digest for every request; the rebind-conflict refusal stays
where it is. Both stay proven.

What that leaves per request in `scope`: the insert under the write lock, the
four revokes (0.050), the plugin map and `Ctx` (0.154) -- and the eight-span
`verify` cost goes with the hash.

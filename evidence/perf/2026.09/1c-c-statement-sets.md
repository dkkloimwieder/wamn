# 1c-c — statement sets verified once per digest

The cut `1c-b-scope-split.md` named: the per-statement SHA-256 and the lowering
move out of the request into the per-digest preparation, beside the
`InstancePre`. A request binds the prepared sets under its scope by `Arc`.

| | |
|---|---|
| source commit | `113691fa` as measured; rebased before landing |
| bead | `wamn-0h0g.17.25` |
| load at launch | 6.25, 6.8 at the end |

## The split, one layer down

`bind_statement_operation` becomes `prepare_statement_set` (verify every digest,
seal into an opaque `PreparedStatementSet`; once per digest) and
`bind_prepared_statement_operation` (insert the shared `Arc` under a scope, keep
the rebind-conflict refusal; once per request). The `PreparedComponent` carries
the admitted operations map and every operation's prepared set; a hit compares
the request's operations against it, so a digest readmitted under different
statements or dependency pins rebuilds instead of reusing.

**Where the refusal fires now.** A digest whose SQL does not hash to its
digest is refused at preparation -- under the readiness preload, once, before
the host reports ready -- rather than on every request. The partial-bind test
proves nothing partial can be bound because nothing is bound before the
refusal.

## Result

Hot means over four samples, per request. The `1c-b` column is the same
machine one run earlier at load 2.0 rising to 6.8; the InstancePre column is
load 11.1.

| span | InstancePre | 1c-b split | **1c-c cut** |
|---|---:|---:|---:|
| `wamn.component.linker_setup` | 1.784 | 2.440 | **0.827** |
| `wamn.linker.scope` | 1.330 | 1.964 | **0.415** |
| `wamn.linker.pending_scope` | 0.969 | 1.644 | **0.153** |
| `wamn.scope.bind` (8 inserts) | -- | 1.395 | 0.055 |
| `wamn.scope.verify` | -- | 0.845 | gone from the request |
| `wamn.scope.lower` | -- | 0.086 | gone |
| `wamn.scope.clear` | -- | 0.028 | 0.020 |
| `wamn.linker.register` | 0.147 | 0.156 | 0.128 |
| `wamn.linker.ctx` | 0.165 | 0.154 | 0.132 |
| `wamn.linker.revokes` | 0.050 | 0.050 | 0.039 |
| `wamn.component.link` (hit) | 0.090 | 0.104 | 0.255 |
| `wamn.component.instantiate` | 1.826 | 1.765 | 1.631 |

**`pending_scope` lost 1.49 ms and `linker_setup` is under a millisecond.** The
eight statement sets now bind as eight `Arc` inserts under one write lock,
0.055 ms; nothing in the request hashes SQL any more.

**`link` grew 0.15 ms**, and that is the price of the correctness guard: a hit
now compares the request's admitted operations map against the entry's --
every operation's statements included -- and clones a map of eight prepared
sets. That compare is what refuses a digest readmitted under different facts.
It is a string compare, not a hash, and it is a tenth of what it replaced; it
is also the next thing to name if `link` is ever the largest item left.

Since the baseline (`1c-residue-and-scope.md`, load 3.89): `linker_setup` 2.779
-> 0.827 ms, a 3.4x reduction across three commits, each measured.

## Gate

| | |
|---|---|
| overhead ratio | **6.90**, ceiling 12 (8.25 last run, 6.49 the run before) |
| recovery after restart | 76 s of 120 |
| journey | pass, all three arms |
| steady receipt | `linker_setup_ms=0.861 link_ms=0.312 instantiate_ms=1.531` |

Three runs at 6.49, 8.25 and 6.90 against one ceiling say what the ratio's
load tolerance is worth: about ±1 between runs on a shared machine. A ceiling
of 8 would have gone red on the `1c-b` run, which added eight spans to every
request on purpose; the ratchet number remains the owner's call, and the
spread is now on record for it.

## What is left per request in `linker_setup`

| span | ms |
|---|---:|
| `wamn.linker.scope` | 0.415 |
| -- `wamn.linker.pending_scope` | 0.153 |
| -- `wamn.linker.ctx` | 0.132 |
| -- `wamn.linker.revokes` | 0.039 |
| `wamn.linker.register` | 0.128 |
| `wamn.linker.hosts` | 0.024 |
| unnamed | 0.26 |

Nothing left in it is per digest. The registration half is a no-op with a warn
(`wamn-0h0g.17.24`); `ctx` is the plugin map and store data; the rest is the
store's own construction. `instantiate` at 1.6 ms is now twice `linker_setup`
and is the largest platform-owned item in the request.

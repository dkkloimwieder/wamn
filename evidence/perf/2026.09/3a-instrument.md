# Fix 3a — instrument every unmeasured gap

**Source commits:** `a0f557fd`, `ea614b5c` on main (authored on `perf/3a-instrument` off `1106dc4e`)  
**Measured:** 2026-09-04T10:44:49-04:00  
**Load average at measurement:** 7.40, 6.29, 4.72  
**Data:** `docs/perf/2026.09/3a-instrument/`

Instrumentation only — no behaviour changed. The point was to see the 7–9 ms
unspanned residue that fix 1 left as the largest phase of a hot request.

## Spans added

| span | site | measured |
|---|---|---:|
| `wamn.route.match` | `routing::Host::routes` | 0.09 ms |
| `wamn.route.validate_input` | `routing::Host::validate_input` | 0.28 ms |
| `wamn.route.permit` | `routing::Host::try_acquire` | 0.03 ms |
| `wamn.postgres.bind_claims` | the `CLAIM_SQL` round trip | 0.45–0.89 ms |
| `wamn.postgres.decode_rows` | `run_verified_query` row loop | 0.08 ms |
| `wamn.postgres.commit` | the implicit transaction's COMMIT | **2.13–3.77 ms** |

## The finding

**The gaps that were named are noise. The one nobody named is the largest item in the
database call.**

Route match, input validation and permit total **0.40 ms** — they were never the residue.

`wamn.postgres` was 4.25 ms with children summing to 1.64 ms. The missing 2.6 ms is a
**second server round trip that carried no span at all**: claim binding opens a
transaction (`BEGIN` pipelined with the bound `CLAIM_SQL`), so every request must then
COMMIT it.

| inside `wamn.postgres` (hot 3) | ms | share |
|---|---:|---:|
| **commit** | **2.129** | **56 %** |
| statement | 0.605 | 16 % |
| bind_claims | 0.447 | 12 % |
| jetstream | 0.141 | 4 % |
| acquire | 0.140 | 4 % |
| decode_rows | 0.075 | 2 % |
| residue | 0.334 | 9 % |

**The COMMIT costs 3.5x the statement it commits.** Two round trips exist purely to carry
session claims that the statement itself takes 0.6 ms to run.

## Phase breakdown (ms)

| trace | auth | resolve | linker | link | inst | db | bind | sql | COMMIT | UNSPANNED | handle_http | ratio |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 3a hot 2 | 2.716 | 0.059 |  2.695 | 0.138 | 1.118 | 5.514 | 0.48 | 0.565 | 3.773 | 7.145 | **19.654** | 11.679 |
| 3a hot 3 | 3.077 | 0.054 |  2.916 | 0.151 | 1.149 | 3.796 | 0.447 | 0.605 | 2.129 | 6.631 | **18.181** | 10.362 |
| 3a hot 4 | 2.912 | 0.081 |  2.845 | 0.2 | 1.949 | 6.785 | 0.889 | 0.923 | 3.407 | 8.51 | **23.589** | 8.212 |
| 3a hot 5 | 3.742 | 0.086 |  3.463 | 0.205 | 1.456 | 4.729 | 0.591 | 0.739 | 2.36 | 9.345 | **23.387** | 10.655 |

## Where the residue still is

The residue barely moved (7–9 ms → 6.6–9.3 ms) because the new route spans are tiny and
the new Postgres spans sit *inside* `wamn.postgres`, which was already counted. Measured
from the hot-3 timeline:

| gap | ms | what |
|---|---:|---|
| start → `route.match` | ~1.8 | request body read — `wash-runtime`, not our tree |
| inside `component.invoke` | ~1.9 | guest execution around the DB call |
| `invoke` end → `handle_http` end | ~1.4 | response serialization — `wash-runtime` |
| inter-span | ~1.5 | scheduling between awaits |

**The two `wash-runtime` gaps cannot be spanned from this repo.** Closing them needs a
fork patch, which carries a ledger row under the standing rule that a fork patch lands
with its ledger entry in the same change.

## What 3b should target, by size

| target | ms | fixable here |
|---|---:|---|
| `postgres.commit` + `bind_claims` | 2.6–4.7 | yes — two round trips for session claims |
| `linker_setup` | 2.7–3.5 | yes — fix 1b |
| `authenticate` | 2.7–3.7 | yes — fix 2 |
| body read + serialization | ~3.2 | fork patch |
| guest execution | ~1.9 | guest code |

## Implementation note

`bind_claims`, `decode_rows` and `commit` are instrumented **async blocks**, not
`.entered()` guards: a span guard held across an `await` makes the future non-`Send`, and
the WIT host functions require `Send`. The first attempt broke three call sites that way.

The first pass also spanned `run_query`'s decode loop and measured nothing, because a
released route takes `run_verified_query`.

## Span trees

### cold-c0000000000000000000000000000011

```
  spans=21
  handle_http_request  125.198 ms  @+0 ms
    invoke_component_handler  124.958 ms  @+0.1 ms
      wamn.route.match  0.079 ms  @+1.7 ms
      wamn.route.authenticate  58.607 ms  @+2.3 ms
        wamn.postgres.acquire  53.374 ms  @+5.5 ms
      wamn.route.validate_input  0.353 ms  @+61.4 ms
      wamn.route.permit  0.048 ms  @+61.8 ms
      wamn.jetstream  3.298 ms  @+62.3 ms
      wamn.router.resolve  0.086 ms  @+65.7 ms
  wamn.component.invoke  57.673 ms  @+65.9 ms
    wamn.component.cache_hit  0.027 ms  @+66.3 ms
    wamn.component.linker_setup  2.318 ms  @+66.4 ms
    wamn.component.link  0.174 ms  @+68.8 ms
    wamn.component.instantiate  1.055 ms  @+69 ms
    wamn.postgres  51.89 ms  @+70.7 ms
      wamn.postgres.acquire  46.965 ms  @+70.8 ms
      wamn.postgres.bind_claims  0.35 ms  @+118.2 ms
      wamn.postgres.statement  1.282 ms  @+118.9 ms
        wamn.postgres.decode_rows  0.101 ms  @+120 ms
      wamn.postgres.commit  2.358 ms  @+120.2 ms
      wamn.jetstream  0.143 ms  @+123.8 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20002

```
  spans=21
  handle_http_request  19.654 ms  @+0 ms
    invoke_component_handler  19.459 ms  @+0.1 ms
      wamn.route.match  0.099 ms  @+1.9 ms
      wamn.route.authenticate  2.716 ms  @+2.6 ms
        wamn.postgres.acquire  0.115 ms  @+4.1 ms
      wamn.route.validate_input  0.137 ms  @+5.7 ms
      wamn.route.permit  0.033 ms  @+5.9 ms
      wamn.jetstream  0.131 ms  @+6.2 ms
      wamn.router.resolve  0.059 ms  @+6.4 ms
  wamn.component.invoke  11.59 ms  @+6.5 ms
    wamn.component.cache_hit  0.024 ms  @+6.9 ms
    wamn.component.linker_setup  2.695 ms  @+7 ms
    wamn.component.link  0.138 ms  @+9.7 ms
    wamn.component.instantiate  1.118 ms  @+9.9 ms
    wamn.postgres  5.514 ms  @+11.7 ms
      wamn.postgres.acquire  0.156 ms  @+11.8 ms
      wamn.postgres.bind_claims  0.48 ms  @+12 ms
      wamn.postgres.statement  0.565 ms  @+12.8 ms
        wamn.postgres.decode_rows  0.085 ms  @+13.2 ms
      wamn.postgres.commit  3.773 ms  @+13.4 ms
      wamn.jetstream  0.211 ms  @+18.5 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20003

```
  spans=21
  handle_http_request  18.181 ms  @+0 ms
    invoke_component_handler  17.977 ms  @+0.1 ms
      wamn.route.match  0.091 ms  @+1.8 ms
      wamn.route.authenticate  3.077 ms  @+2.4 ms
        wamn.postgres.acquire  0.104 ms  @+4.2 ms
      wamn.route.validate_input  0.283 ms  @+5.8 ms
      wamn.route.permit  0.032 ms  @+6.1 ms
      wamn.jetstream  0.124 ms  @+6.5 ms
      wamn.router.resolve  0.054 ms  @+6.7 ms
  wamn.component.invoke  9.988 ms  @+6.8 ms
    wamn.component.cache_hit  0.028 ms  @+7.1 ms
    wamn.component.linker_setup  2.916 ms  @+7.3 ms
    wamn.component.link  0.151 ms  @+10.2 ms
    wamn.component.instantiate  1.149 ms  @+10.4 ms
    wamn.postgres  3.796 ms  @+12.2 ms
      wamn.postgres.acquire  0.14 ms  @+12.2 ms
      wamn.postgres.bind_claims  0.447 ms  @+12.4 ms
      wamn.postgres.statement  0.605 ms  @+13.1 ms
        wamn.postgres.decode_rows  0.075 ms  @+13.6 ms
      wamn.postgres.commit  2.129 ms  @+13.8 ms
      wamn.jetstream  0.141 ms  @+17 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20004

```
  spans=21
  handle_http_request  23.589 ms  @+0 ms
    invoke_component_handler  23.244 ms  @+0.2 ms
      wamn.route.match  0.104 ms  @+1.9 ms
      wamn.route.authenticate  2.912 ms  @+2.6 ms
        wamn.postgres.acquire  0.124 ms  @+4.1 ms
      wamn.route.validate_input  0.156 ms  @+6 ms
      wamn.route.permit  0.046 ms  @+6.3 ms
      wamn.jetstream  0.159 ms  @+6.7 ms
      wamn.router.resolve  0.081 ms  @+6.9 ms
  wamn.component.invoke  14.594 ms  @+7.1 ms
    wamn.component.cache_hit  0.032 ms  @+7.6 ms
    wamn.component.linker_setup  2.845 ms  @+7.8 ms
    wamn.component.link  0.2 ms  @+10.7 ms
    wamn.component.instantiate  1.949 ms  @+11 ms
    wamn.postgres  6.785 ms  @+14 ms
      wamn.postgres.acquire  0.276 ms  @+14.2 ms
      wamn.postgres.bind_claims  0.889 ms  @+14.6 ms
      wamn.postgres.statement  0.923 ms  @+16.4 ms
        wamn.postgres.decode_rows  0.14 ms  @+17.1 ms
      wamn.postgres.commit  3.407 ms  @+17.4 ms
      wamn.jetstream  0.154 ms  @+22 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20005

```
  spans=21
  handle_http_request  23.387 ms  @+0 ms
    invoke_component_handler  23.108 ms  @+0.2 ms
      wamn.route.match  0.161 ms  @+2.8 ms
      wamn.route.authenticate  3.742 ms  @+3.8 ms
        wamn.postgres.acquire  0.166 ms  @+5.9 ms
      wamn.route.validate_input  0.151 ms  @+8 ms
      wamn.route.permit  0.05 ms  @+8.3 ms
      wamn.jetstream  0.155 ms  @+8.7 ms
      wamn.router.resolve  0.086 ms  @+8.9 ms
  wamn.component.invoke  12.408 ms  @+9.1 ms
    wamn.component.cache_hit  0.027 ms  @+9.6 ms
    wamn.component.linker_setup  3.463 ms  @+9.8 ms
    wamn.component.link  0.205 ms  @+13.3 ms
    wamn.component.instantiate  1.456 ms  @+13.6 ms
    wamn.postgres  4.729 ms  @+15.8 ms
      wamn.postgres.acquire  0.19 ms  @+15.8 ms
      wamn.postgres.bind_claims  0.591 ms  @+16.1 ms
      wamn.postgres.statement  0.739 ms  @+17.3 ms
        wamn.postgres.decode_rows  0.087 ms  @+17.9 ms
      wamn.postgres.commit  2.36 ms  @+18 ms
      wamn.jetstream  0.188 ms  @+21.9 ms
```

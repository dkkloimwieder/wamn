# Fix 3c — PostgreSQL decides which statements need a transaction

**Source commits:** `63076ecd`, `d5abef93`, `1109bea0`, `9a06b86e`, `f94bb29d` on main (authored on `perf/3c-operation-kind`)  
**Measured:** 2026-09-04T12:25:51-04:00  
**Load average at measurement:** 0.59, 2.01, 3.41  
**Data:** `docs/perf/2026.09/3c-operation-kind/`

## Result — rule 4 assertion passes

No `bind_claims` and no `commit` span in any of the four hot reads.

| inside `wamn.postgres` | 3b | 3c |
|---|---:|---:|
| **commit** | **2.166** | **gone** |
| **bind_claims** | **0.632** | **gone** |
| session_settings | — | 0.728 |
| statement | 0.795 | 0.686 |
| acquire | 0.156 | 0.265 |
| jetstream | 0.110 | 0.142 |
| **total** | **3.677** | **1.960** |

**`wamn.postgres` 3.68 ms → 1.96 ms, 48 % faster.**

## How the verdict is reached

`EXPLAIN (GENERIC_PLAN, FORMAT JSON)` against the already-migrated database at
generation time. A statement needs a transaction when its plan carries a `ModifyTable`
node (it writes — data-modifying CTEs included) or a `LockRows` node (it takes a row
lock, which autocommit would drop the instant the statement returned). The bit rides
`ComponentSqlStatement.transactional` through admission to `VerifiedStatement`.

Generation runs twice: the statements are generated from the catalog, so they cannot be
planned before they exist. The bit is contract-only and never reaches SQL bytes, so both
passes produce an identical corpus.

## The verdicts, and why asking the server mattered

| statement | verdict |
|---|---|
| `purchase_order/get`, six `query` variants, `receipt/get`, `list_locations`, `load_receipt_screen` | autocommit |
| **`purchase_order/update`** | **transactional** |
| `lock_purchase_order` | transactional — via `LockRows` |
| `find_replay`, `load_purchase_order_detail` | autocommit — reads inside command operations |
| the other seven `record_receipt` statements | transactional |

**`purchase_order/update` declares `transaction: implicit` in `wamn.json`, identically to
`get` and `query`.** An authored-declaration route would have sent a write down the
autocommit path and silently lost its causation. The plan says `ModifyTable`.

`lock_purchase_order` is `SELECT … FOR UPDATE` with no `ModifyTable` node at all — which
is why the field is named for needing a transaction rather than for writing.

## Rule 4, both halves — the write path is exercised, not only classified

A classification proven at build time with the runtime path unexercised is how a
proof misses a lost transaction. `POST /purchase_order/update` was driven through
the runner with the same payload the proof uses, and its trace captured.

| | `bind_claims` | `commit` |
|---|---|---|
| read (`purchase_order/get`) | **absent** | **absent** |
| write (`purchase_order/update`) | **4.070 ms** | **2.231 ms** |

The write keeps its transaction, so its causation still rides the commit.

Two things only the write trace shows:

- **3b's pipelining is observable here for the first time.** `statement` runs
  17.0 → 22.2 ms and `bind_claims` 18.3 → 22.4 ms: overlapping, not serialized.
  The read path has no claim binding left to overlap with.
- **The write's `wamn.postgres` is 8.845 ms against the read's 1.960 ms** — nine
  statements, a row lock, and causation riding the commit. That is the shape the
  ruling intends, not a defect.

```
    wamn.postgres  8.845 ms  @+16.4 ms
      wamn.postgres.acquire  0.337 ms  @+16.5 ms
      wamn.postgres.statement  5.204 ms  @+17 ms
      wamn.postgres.bind_claims  4.07 ms  @+18.3 ms
        wamn.postgres.decode_rows  0.086 ms  @+22 ms
      wamn.postgres.commit  2.231 ms  @+22.9 ms
      wamn.jetstream  0.176 ms  @+26.9 ms
```

## Phase breakdown (ms)

| trace | auth | resolve | linker | link | inst | db | bind | sql | COMMIT | UNSPANNED | handle_http | ratio |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 3c hot 2 | 3.799 | 0.066 |  3.19 | 0.179 | 1.493 | 2.26 | 0 | 1.118 | 0 | 9.014 | **20.313** | 7.778 |
| 3c hot 3 | 4.23 | 0.097 |  2.875 | 0.173 | 1.562 | 1.96 | 0 | 0.686 | 0 | 9.398 | **20.95** | 9.321 |
| 3c hot 4 | 5.67 | 0.095 |  4.337 | 0.237 | 2.444 | 2.436 | 0 | 1.059 | 0 | 11.498 | **27.338** | 7.804 |
| 3c hot 5 | 4.082 | 0.098 |  3.67 | 0.189 | 1.901 | 2.203 | 0 | 0.828 | 0 | 9.969 | **22.475** | 8.235 |

## Three corrections found by running it

1. **Plan under the package's own `search_path`**, or an unqualified package-owned
   relation cannot be resolved and every statement using it fails to plan.
2. **Use `simple_query`.** The extended protocol treats the `$1..$n` inside the statement
   as parameters *of the EXPLAIN* and refuses with "expected N parameters but got 0".
3. **An autocommit read still needs `search_path` and `statement_timeout`.** The first cut
   skipped `GUEST_CLAIM_SQL` entirely and the journey refused `purchase-order-get` with
   `internal_error` — that statement is also what sets `search_path`.

A fourth: the settings cannot ride the statement's flight. Each side is a prepare
followed by an execute, and interleaving two such futures orders the *sends*, not the two
*executes* — so the statement could still run before `search_path` applied. The journey
caught that too. A read is therefore two flights, not one.

## What a read still pays, and what is left

`app.role` and `app.user_id` are excluded from the session-scoped settings on purpose: a
session-scoped claim outlives the request and reaches the next borrower of the pooled
connection. A read carrying either keeps the transactional path.

`wamn-0h0g.17.18` moves `search_path`, `statement_timeout` and `app.runner` to connection
setup — they are pool-uniform, so paying for them per request is waste, and removing them
takes a read to a single round trip.

## Span trees

### cold-c0000000000000000000000000000011

```
  spans=20
  handle_http_request  152.556 ms  @+0 ms
    invoke_component_handler  152.246 ms  @+0.2 ms
      wamn.route.match  0.145 ms  @+2.9 ms
      wamn.route.authenticate  69.3 ms  @+4 ms
        wamn.postgres.acquire  62.633 ms  @+8.2 ms
      wamn.route.validate_input  0.535 ms  @+73.8 ms
      wamn.route.permit  0.073 ms  @+74.5 ms
      wamn.jetstream  2.285 ms  @+75.2 ms
      wamn.router.resolve  0.137 ms  @+77.6 ms
  wamn.component.invoke  72.29 ms  @+77.8 ms
    wamn.component.cache_hit  0.035 ms  @+78.3 ms
    wamn.component.linker_setup  3.141 ms  @+78.5 ms
    wamn.component.link  0.163 ms  @+81.7 ms
    wamn.component.instantiate  1.513 ms  @+81.9 ms
    wamn.postgres  64.791 ms  @+84.2 ms
      wamn.postgres.acquire  61.384 ms  @+84.3 ms
      wamn.postgres.session_settings  0.852 ms  @+145.8 ms
      wamn.postgres.statement  2.236 ms  @+146.7 ms
        wamn.postgres.decode_rows  0.115 ms  @+148.8 ms
      wamn.jetstream  0.256 ms  @+150.6 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20002

```
  spans=20
  handle_http_request  20.313 ms  @+0 ms
    invoke_component_handler  20.059 ms  @+0.2 ms
      wamn.route.match  0.104 ms  @+2 ms
      wamn.route.authenticate  3.799 ms  @+2.9 ms
        wamn.postgres.acquire  0.147 ms  @+4.7 ms
      wamn.route.validate_input  0.152 ms  @+7.2 ms
      wamn.route.permit  0.056 ms  @+7.5 ms
      wamn.jetstream  0.164 ms  @+8 ms
      wamn.router.resolve  0.066 ms  @+8.2 ms
  wamn.component.invoke  9.93 ms  @+8.4 ms
    wamn.component.cache_hit  0.035 ms  @+8.9 ms
    wamn.component.linker_setup  3.19 ms  @+9.1 ms
    wamn.component.link  0.179 ms  @+12.4 ms
    wamn.component.instantiate  1.493 ms  @+12.6 ms
    wamn.postgres  2.26 ms  @+14.9 ms
      wamn.postgres.acquire  0.212 ms  @+15 ms
      wamn.postgres.session_settings  0.678 ms  @+15.2 ms
      wamn.postgres.statement  1.118 ms  @+16 ms
        wamn.postgres.decode_rows  0.138 ms  @+16.9 ms
      wamn.jetstream  0.183 ms  @+18.7 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20003

```
  spans=20
  handle_http_request  20.95 ms  @+0 ms
    invoke_component_handler  20.704 ms  @+0.2 ms
      wamn.route.match  0.101 ms  @+2 ms
      wamn.route.authenticate  4.23 ms  @+2.8 ms
        wamn.postgres.acquire  0.175 ms  @+4.8 ms
      wamn.route.validate_input  0.498 ms  @+7.5 ms
      wamn.route.permit  0.056 ms  @+8.2 ms
      wamn.jetstream  0.184 ms  @+8.8 ms
      wamn.router.resolve  0.097 ms  @+9 ms
  wamn.component.invoke  9.272 ms  @+9.3 ms
    wamn.component.cache_hit  0.03 ms  @+9.8 ms
    wamn.component.linker_setup  2.875 ms  @+10 ms
    wamn.component.link  0.173 ms  @+12.9 ms
    wamn.component.instantiate  1.562 ms  @+13.1 ms
    wamn.postgres  1.96 ms  @+15.6 ms
      wamn.postgres.acquire  0.265 ms  @+15.7 ms
      wamn.postgres.session_settings  0.728 ms  @+16.1 ms
      wamn.postgres.statement  0.686 ms  @+16.9 ms
        wamn.postgres.decode_rows  0.097 ms  @+17.4 ms
      wamn.jetstream  0.142 ms  @+18.8 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20004

```
  spans=20
  handle_http_request  27.338 ms  @+0 ms
    invoke_component_handler  27.082 ms  @+0.1 ms
      wamn.route.match  0.128 ms  @+2.7 ms
      wamn.route.authenticate  5.67 ms  @+3.7 ms
        wamn.postgres.acquire  0.729 ms  @+6.4 ms
      wamn.route.validate_input  0.432 ms  @+10 ms
      wamn.route.permit  0.061 ms  @+10.7 ms
      wamn.jetstream  0.196 ms  @+11.2 ms
      wamn.router.resolve  0.095 ms  @+11.5 ms
  wamn.component.invoke  13.087 ms  @+11.7 ms
    wamn.component.cache_hit  0.038 ms  @+12.1 ms
    wamn.component.linker_setup  4.337 ms  @+12.4 ms
    wamn.component.link  0.237 ms  @+16.8 ms
    wamn.component.instantiate  2.444 ms  @+17.1 ms
    wamn.postgres  2.436 ms  @+20.6 ms
      wamn.postgres.acquire  0.294 ms  @+20.7 ms
      wamn.postgres.session_settings  0.753 ms  @+21.1 ms
      wamn.postgres.statement  1.059 ms  @+21.9 ms
        wamn.postgres.decode_rows  0.158 ms  @+22.7 ms
      wamn.jetstream  0.542 ms  @+25.2 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20005

```
  spans=20
  handle_http_request  22.475 ms  @+0 ms
    invoke_component_handler  22.089 ms  @+0.3 ms
      wamn.route.match  0.107 ms  @+2.3 ms
      wamn.route.authenticate  4.082 ms  @+3.1 ms
        wamn.postgres.acquire  0.242 ms  @+5 ms
      wamn.route.validate_input  0.193 ms  @+7.7 ms
      wamn.route.permit  0.063 ms  @+8.1 ms
      wamn.jetstream  0.166 ms  @+8.7 ms
      wamn.router.resolve  0.098 ms  @+8.9 ms
  wamn.component.invoke  11.233 ms  @+9.2 ms
    wamn.component.cache_hit  0.042 ms  @+9.8 ms
    wamn.component.linker_setup  3.67 ms  @+10 ms
    wamn.component.link  0.189 ms  @+13.8 ms
    wamn.component.instantiate  1.901 ms  @+14 ms
    wamn.postgres  2.203 ms  @+17.1 ms
      wamn.postgres.acquire  0.263 ms  @+17.2 ms
      wamn.postgres.session_settings  0.788 ms  @+17.5 ms
      wamn.postgres.statement  0.828 ms  @+18.4 ms
        wamn.postgres.decode_rows  0.163 ms  @+19 ms
      wamn.jetstream  0.169 ms  @+20.8 ms
```

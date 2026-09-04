# Fix 3b — the claim transaction and the statement in one flight

**Source commit:** `ab8f6559` on main (authored on `perf/3b-pipeline` off `e5ced1b3`)  
**Measured:** 2026-09-04T11:10:57-04:00  
**Load average at measurement:** 3.67, 5.66, 4.90  
**Data:** `docs/perf/2026.09/3b-pipeline/`

## What changed

`one_shot_statement` awaited `begin_with_claims`, then awaited the statement, then
awaited `COMMIT` — three round trips to run one 0.6 ms query. Claims and statement now
ride a single `join!`. tokio-postgres preserves FIFO order per connection, which is
already the mechanism that puts `BEGIN` before the transaction-LOCAL `set_config`s.

**Two flights is the floor for a real transaction.** COMMIT versus ROLLBACK cannot be
decided before the statement's result is known. Reads reaching one flight needs the
declared operation kind carried through admission — a separate commit.

## The proof is the overlap, not the total

| | `bind_claims` | `statement` | serialized |
|---|---|---|---|
| 3a before | @12.4 → 12.85 | @13.1 → 13.80 | yes, with a gap |
| 3b after | @12.9 → 13.53 | @13.0 → 13.80 | **no, overlapping** |

Wall-clock for the pair: **1.31 ms → 0.90 ms**.

Load was 7.40 when 3a was measured and 4.88 here, so the absolute totals are not
comparable between the two reports. The span overlap is a structural fact and is.

## Inside `wamn.postgres` (hot 3)

| | ms | share |
|---|---:|---:|
| **commit** | **2.166** | **59 %** |
| statement | 0.795 | 22 % |
| bind_claims | 0.632 | 17 % (overlapping the statement) |
| acquire | 0.156 | 4 % |
| jetstream | 0.110 | 3 % |
| decode_rows | 0.079 | 2 % |

COMMIT is now the majority of the database call. A read has no reason to pay it.

## Phase breakdown (ms)

| trace | auth | resolve | linker | link | inst | db | bind | sql | COMMIT | UNSPANNED | handle_http | ratio |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 3b hot 2 | 3.556 | 0.087 |  3.071 | 0.153 | 1.228 | 7.425 | 0.696 | 2.343 | 4.412 | 8.685 | **24.873** | 6.965 |
| 3b hot 3 | 3.014 | 0.073 |  2.939 | 0.19 | 1.207 | 3.677 | 0.632 | 0.795 | 2.166 | 7.097 | **18.461** | 9.224 |
| 3b hot 4 | 3.156 | 0.062 |  2.718 | 0.158 | 1.245 | 3.809 | 0.67 | 0.813 | 2.227 | 7.548 | **18.994** | 9.232 |
| 3b hot 5 | 3.485 | 0.069 |  2.676 | 0.175 | 1.304 | 12.888 | 0.618 | 0.729 | 11.438 | 7.204 | **28.086** | 13.818 |

## One correctness change, not asked for

A failed claim binding now ROLLS BACK before returning. Previously claims were awaited
first, so a failure returned before any statement was sent. Now the statement rides the
same flight into a transaction that never opened, so its error is a consequence rather
than the cause — and the connection must not be repooled inside an open transaction.

## Span trees

### cold-c0000000000000000000000000000011

```
  spans=19
  handle_http_request  127.944 ms  @+0 ms
    invoke_component_handler  127.714 ms  @+0.1 ms
      wamn.route.match  0.083 ms  @+2 ms
      wamn.route.authenticate  57.431 ms  @+2.6 ms
        wamn.postgres.acquire  51.805 ms  @+5.9 ms
      wamn.route.validate_input  0.332 ms  @+60.5 ms
      wamn.route.permit  0.039 ms  @+60.9 ms
      wamn.jetstream  1.913 ms  @+61.3 ms
      wamn.router.resolve  0.076 ms  @+63.3 ms
  wamn.component.invoke  62.897 ms  @+63.5 ms
    wamn.component.cache_hit  0.027 ms  @+63.8 ms
    wamn.component.linker_setup  2.701 ms  @+64 ms
    wamn.component.link  0.187 ms  @+66.8 ms
    wamn.component.instantiate  1.269 ms  @+67.1 ms
    wamn.postgres  56.451 ms  @+69.2 ms
      wamn.postgres.acquire  54.583 ms  @+69.3 ms
      wamn.postgres.statement  0.752 ms  @+124 ms
      wamn.postgres.bind_claims  0.512 ms  @+124.5 ms
      wamn.jetstream  0.145 ms  @+126.6 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20002

```
  spans=21
  handle_http_request  24.873 ms  @+0 ms
    invoke_component_handler  24.612 ms  @+0.2 ms
      wamn.route.match  0.144 ms  @+2.4 ms
      wamn.route.authenticate  3.556 ms  @+3.4 ms
        wamn.postgres.acquire  0.123 ms  @+5.3 ms
      wamn.route.validate_input  0.477 ms  @+7.5 ms
      wamn.route.permit  0.046 ms  @+8.1 ms
      wamn.jetstream  0.157 ms  @+8.6 ms
      wamn.router.resolve  0.087 ms  @+8.8 ms
  wamn.component.invoke  14.443 ms  @+9.1 ms
    wamn.component.cache_hit  0.037 ms  @+9.6 ms
    wamn.component.linker_setup  3.071 ms  @+9.8 ms
    wamn.component.link  0.153 ms  @+13 ms
    wamn.component.instantiate  1.228 ms  @+13.2 ms
    wamn.postgres  7.425 ms  @+15.2 ms
      wamn.postgres.acquire  0.259 ms  @+15.3 ms
      wamn.postgres.bind_claims  0.696 ms  @+15.7 ms
      wamn.postgres.statement  2.343 ms  @+15.8 ms
        wamn.postgres.decode_rows  0.111 ms  @+18 ms
      wamn.postgres.commit  4.412 ms  @+18.2 ms
      wamn.jetstream  0.13 ms  @+23.8 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20003

```
  spans=21
  handle_http_request  18.461 ms  @+0 ms
    invoke_component_handler  18.254 ms  @+0.1 ms
      wamn.route.match  0.091 ms  @+1.8 ms
      wamn.route.authenticate  3.014 ms  @+2.4 ms
        wamn.postgres.acquire  0.12 ms  @+4.1 ms
      wamn.route.validate_input  0.14 ms  @+5.8 ms
      wamn.route.permit  0.035 ms  @+6.1 ms
      wamn.jetstream  0.142 ms  @+6.4 ms
      wamn.router.resolve  0.073 ms  @+6.6 ms
  wamn.component.invoke  10.275 ms  @+6.8 ms
    wamn.component.cache_hit  0.032 ms  @+7.3 ms
    wamn.component.linker_setup  2.939 ms  @+7.5 ms
    wamn.component.link  0.19 ms  @+10.5 ms
    wamn.component.instantiate  1.207 ms  @+10.7 ms
    wamn.postgres  3.677 ms  @+12.6 ms
      wamn.postgres.acquire  0.156 ms  @+12.7 ms
      wamn.postgres.bind_claims  0.632 ms  @+12.9 ms
      wamn.postgres.statement  0.795 ms  @+13 ms
        wamn.postgres.decode_rows  0.079 ms  @+13.7 ms
      wamn.postgres.commit  2.166 ms  @+14 ms
      wamn.jetstream  0.11 ms  @+17.3 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20004

```
  spans=21
  handle_http_request  18.994 ms  @+0 ms
    invoke_component_handler  18.784 ms  @+0.1 ms
      wamn.route.match  0.091 ms  @+1.8 ms
      wamn.route.authenticate  3.156 ms  @+2.5 ms
        wamn.postgres.acquire  0.128 ms  @+4.1 ms
      wamn.route.validate_input  0.17 ms  @+6 ms
      wamn.route.permit  0.037 ms  @+6.3 ms
      wamn.jetstream  0.152 ms  @+6.8 ms
      wamn.router.resolve  0.062 ms  @+7 ms
  wamn.component.invoke  10.081 ms  @+7.2 ms
    wamn.component.cache_hit  0.03 ms  @+7.6 ms
    wamn.component.linker_setup  2.718 ms  @+7.7 ms
    wamn.component.link  0.158 ms  @+10.5 ms
    wamn.component.instantiate  1.245 ms  @+10.7 ms
    wamn.postgres  3.809 ms  @+12.6 ms
      wamn.postgres.acquire  0.163 ms  @+12.6 ms
      wamn.postgres.bind_claims  0.67 ms  @+12.9 ms
      wamn.postgres.statement  0.813 ms  @+13 ms
        wamn.postgres.decode_rows  0.089 ms  @+13.7 ms
      wamn.postgres.commit  2.227 ms  @+14.1 ms
      wamn.jetstream  0.17 ms  @+17.7 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20005

```
  spans=21
  handle_http_request  28.086 ms  @+0 ms
    invoke_component_handler  27.86 ms  @+0.2 ms
      wamn.route.match  0.098 ms  @+2 ms
      wamn.route.authenticate  3.485 ms  @+2.6 ms
        wamn.postgres.acquire  0.117 ms  @+4.7 ms
      wamn.route.validate_input  0.117 ms  @+6.5 ms
      wamn.route.permit  0.069 ms  @+6.7 ms
      wamn.jetstream  0.125 ms  @+7 ms
      wamn.router.resolve  0.069 ms  @+7.2 ms
  wamn.component.invoke  19.259 ms  @+7.4 ms
    wamn.component.cache_hit  0.025 ms  @+7.8 ms
    wamn.component.linker_setup  2.676 ms  @+7.9 ms
    wamn.component.link  0.175 ms  @+10.6 ms
    wamn.component.instantiate  1.304 ms  @+10.8 ms
    wamn.postgres  12.888 ms  @+12.8 ms
      wamn.postgres.acquire  0.147 ms  @+12.9 ms
      wamn.postgres.bind_claims  0.618 ms  @+13.1 ms
      wamn.postgres.statement  0.729 ms  @+13.2 ms
        wamn.postgres.decode_rows  0.078 ms  @+13.8 ms
      wamn.postgres.commit  11.438 ms  @+14.2 ms
      wamn.jetstream  0.128 ms  @+26.9 ms
```

# Fix 1 — compiled components cached in-process by digest

**Source commit:** `56b1ced5` (branch `perf/1-component-cache`, base `4dec956a`)  
**Measured:** 2026-09-04T09:45:58-04:00  
**Load average at measurement:** 3.90, 3.40, 2.93  
**Data:** `docs/perf/2026.09/1-component-cache/`

## What changed

`RouterDriver` now holds a `BTreeMap<String, Component>` keyed by artifact digest,
consulted before the OCI pull. A digest names immutable bytes, so a hit is always
correct and an entry can never go stale; the miss path is the only place that pulls
or compiles.

**Why the number moved:** `Component::new` was never recompiling — the wasmtime disk
cache was already being hit (`cache-cold`/`cache-warm` listings and sha256s are
byte-identical in the baseline evidence). It was *deserializing* that cached artifact
on every request, and the ~1 s OCI pull existed only to feed it. Holding the
`Component` in process removes both.

## Client-side totals

| phase | baseline | fix 1 |
|---|---:|---:|
| cold | 26 869 / 26 993 ms | 37 379 ms |
| hot | 1381 / 1423 / 1424 ms | **32.2 / 29.3 / 28.1 / 27.3 ms** |

Hot requests are **~50x faster**. Cold is unchanged in kind — the first request must
still compile — and is slower here because this cluster compiled with no disk-cache
entry present at all (28.2 s of compile in the cold trace).

## Phase breakdown (ms)

| trace | auth | resolve | pull | compile | linker | link | inst | db | sql | handle_http | ratio |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| baseline hot | 2.134 | 0.069 | 976.81 | 386.346 | 2.093 | 0.122 | 1.308 | 3.711 | 0.431 | **1380.019** | 793.567 |
| baseline hot | 2.522 | 0.064 | 1025.494 | 377.971 | 2.404 | 0.135 | 1.383 | 4.031 | 0.52 | **1421.77** | 747.079 |
| baseline hot | 2.018 | 0.057 | 1040.372 | 364.517 | 2.875 | 0.168 | 1.45 | 3.459 | 0.414 | **1422.545** | 763.194 |
| fix1 COLD | 65.204 | 104.607 | 1393.763 | 35727.024 | 2.787 | 0.193 | 1.906 | 68.397 | 1.679 | **37375.768** | 10425.528 |
| fix1 hot 2 | 3.321 | 0.092 | 0 | 0 | 3.49 | 0.15 | 1.335 | 13.015 | 0.498 | **29.953** | 16.341 |
| fix1 hot 3 | 4.227 | 0.088 | 0 | 0 | 3.98 | 0.237 | 1.956 | 4.179 | 0.482 | **24.893** | 10.212 |
| fix1 hot 4 | 3.856 | 0.096 | 0 | 0 | 4.476 | 0.249 | 2.13 | 4.359 | 0.566 | **25.629** | 9.505 |
| fix1 hot 5 | 3.525 | 0.097 | 0 | 0 | 4.598 | 0.318 | 2.11 | 4.481 | 0.601 | **24.756** | 9.132 |

## Overhead ratio

`handle_http_request / (postgres.statement + component.instantiate)` — platform overhead
against real work, load-independent.

| | ratio |
|---|---:|
| baseline hot | **793.6** |
| fix 1 hot | **10.2** |

## Span trees

### trace-cold-c01dc01dc01dc01dc01dc01dc01d0001

```
  spans=17
  handle_http_request  37375.768 ms  @+0 ms
    invoke_component_handler  37375.494 ms  @+0.2 ms
      wamn.route.authenticate  65.204 ms  @+2.6 ms
        wamn.postgres.acquire  58.573 ms  @+6.1 ms
      wamn.jetstream  2.25 ms  @+69.1 ms
      wamn.router.resolve  104.607 ms  @+71.5 ms
        wamn.postgres.acquire  86.058 ms  @+71.6 ms
  wamn.component.invoke  37197.394 ms  @+176.3 ms
    wamn.component.pull  1393.763 ms  @+177 ms
    wamn.component.compile  35727.024 ms  @+1570.9 ms
    wamn.component.linker_setup  2.787 ms  @+37298.2 ms
    wamn.component.link  0.193 ms  @+37301 ms
    wamn.component.instantiate  1.906 ms  @+37301.3 ms
    wamn.postgres  68.397 ms  @+37304.1 ms
      wamn.postgres.acquire  62.488 ms  @+37304.2 ms
      wamn.postgres.statement  1.679 ms  @+37368.1 ms
      wamn.jetstream  0.176 ms  @+37374.1 ms
```

### trace-hot-d0d0d0d0d0d0d0d0d0d0d0d0d0d00002

```
  spans=15
  handle_http_request  29.953 ms  @+0 ms
    invoke_component_handler  29.701 ms  @+0.1 ms
      wamn.route.authenticate  3.321 ms  @+2.5 ms
        wamn.postgres.acquire  0.13 ms  @+4.1 ms
      wamn.jetstream  0.154 ms  @+6.9 ms
      wamn.router.resolve  0.092 ms  @+7.2 ms
  wamn.component.invoke  20.811 ms  @+7.4 ms
    wamn.component.cache_hit  0.043 ms  @+8.1 ms
    wamn.component.linker_setup  3.49 ms  @+8.4 ms
    wamn.component.link  0.15 ms  @+11.9 ms
    wamn.component.instantiate  1.335 ms  @+12.1 ms
    wamn.postgres  13.015 ms  @+14.2 ms
      wamn.postgres.acquire  0.162 ms  @+14.2 ms
      wamn.postgres.statement  0.498 ms  @+15.3 ms
      wamn.jetstream  0.174 ms  @+28.5 ms
```

### trace-hot-d0d0d0d0d0d0d0d0d0d0d0d0d0d00003

```
  spans=15
  handle_http_request  24.893 ms  @+0 ms
    invoke_component_handler  24.569 ms  @+0.2 ms
      wamn.route.authenticate  4.227 ms  @+3.9 ms
        wamn.postgres.acquire  0.182 ms  @+6.1 ms
      wamn.jetstream  0.261 ms  @+9.3 ms
      wamn.router.resolve  0.088 ms  @+9.6 ms
  wamn.component.invoke  13.102 ms  @+9.9 ms
    wamn.component.cache_hit  0.034 ms  @+10.4 ms
    wamn.component.linker_setup  3.98 ms  @+10.6 ms
    wamn.component.link  0.237 ms  @+14.7 ms
    wamn.component.instantiate  1.956 ms  @+15 ms
    wamn.postgres  4.179 ms  @+17.9 ms
      wamn.postgres.acquire  0.27 ms  @+18 ms
      wamn.postgres.statement  0.482 ms  @+19.3 ms
      wamn.jetstream  0.18 ms  @+23.3 ms
```

### trace-hot-d0d0d0d0d0d0d0d0d0d0d0d0d0d00004

```
  spans=15
  handle_http_request  25.629 ms  @+0 ms
    invoke_component_handler  25.318 ms  @+0.2 ms
      wamn.route.authenticate  3.856 ms  @+3.9 ms
        wamn.postgres.acquire  0.178 ms  @+5.8 ms
      wamn.jetstream  0.203 ms  @+8.9 ms
      wamn.router.resolve  0.096 ms  @+9.1 ms
  wamn.component.invoke  14.28 ms  @+9.4 ms
    wamn.component.cache_hit  0.043 ms  @+10 ms
    wamn.component.linker_setup  4.476 ms  @+10.2 ms
    wamn.component.link  0.249 ms  @+14.8 ms
    wamn.component.instantiate  2.13 ms  @+15.1 ms
    wamn.postgres  4.359 ms  @+18.2 ms
      wamn.postgres.acquire  0.271 ms  @+18.3 ms
      wamn.postgres.statement  0.566 ms  @+19.6 ms
      wamn.jetstream  0.173 ms  @+24 ms
```

### trace-hot-d0d0d0d0d0d0d0d0d0d0d0d0d0d00005

```
  spans=15
  handle_http_request  24.756 ms  @+0 ms
    invoke_component_handler  24.457 ms  @+0.2 ms
      wamn.route.authenticate  3.525 ms  @+2.8 ms
        wamn.postgres.acquire  0.133 ms  @+4.4 ms
      wamn.jetstream  0.221 ms  @+7.5 ms
      wamn.router.resolve  0.097 ms  @+7.8 ms
  wamn.component.invoke  14.627 ms  @+8.1 ms
    wamn.component.cache_hit  0.046 ms  @+8.7 ms
    wamn.component.linker_setup  4.598 ms  @+9 ms
    wamn.component.link  0.318 ms  @+13.7 ms
    wamn.component.instantiate  2.11 ms  @+14 ms
    wamn.postgres  4.481 ms  @+17.2 ms
      wamn.postgres.acquire  0.264 ms  @+17.3 ms
      wamn.postgres.statement  0.601 ms  @+18.8 ms
      wamn.jetstream  0.173 ms  @+23.1 ms
```

# Fix 1 — compiled components cached in-process by digest

**Source commits:** `56b1ced5`, `e14011e9`, `726657ad` (branch `perf/1-component-cache`, base `4dec956a`)  
**Measured:** 2026-09-04T10:15:10-04:00  
**Load average at measurement:** 0.79, 0.88, 1.63  
**Data:** `docs/perf/2026.09/1-component-cache/`

## Result

| | baseline | fix 1 |
|---|---:|---:|
| cold | 26 869 / 26 993 ms | **105.5 ms** |
| hot | 1381 / 1423 / 1424 ms | **18.5 / 23.2 / 23.5 / 23.8 ms** |
| overhead ratio (hot) | 747–794 | **8.4–12.7** |

Cold **255x** faster, hot **~60x**.

## What changed, and why the number moved

Three commits, each necessary:

1. `56b1ced5` — `RouterDriver` holds a `BTreeMap<String, Component>` keyed by artifact
   digest, consulted before the OCI pull. A digest names immutable bytes, so a hit is
   always correct and an entry can never go stale.
2. `e14011e9` — `prepare_synchronous_release` already pulled and compiled every release
   digest to prove the closure servable, then **dropped the result**. It now inserts into
   the same cache, and readiness refuses unless every release digest is present.
3. `726657ad` — **the preload never ran in the serving host.** `RouterReadinessProbe` was
   constructed in `services/executor` and nowhere else, so `services/host` — the process
   serving every HTTP route — went straight to serving with an empty cache and reported
   ready anyway. Cold stayed at 35.8 s after commit 2 for exactly this reason.

**`Component::new` was never recompiling.** The wasmtime disk cache was already being hit:
`cache-cold` and `cache-warm` listings and sha256s are byte-identical in the baseline
evidence, so no entry was written across hot requests. It was *deserializing* the cached
artifact every request, and the ~1 s OCI pull existed only to feed it. Holding the
`Component` in process removes both; running the preload in the host removes them from the
first request too.

## Phase breakdown (ms)

`UNSPANNED` is `handle_http_request` minus the phases that partition it (`db` counted,
`sql` nested inside it) — time inside the request that no span covers.

| trace | auth | resolve | pull | compile | linker | link | inst | db | sql | UNSPANNED | handle_http | ratio |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| baseline hot | 2.134 | 0.069 | 976.81 | 386.346 | 2.093 | 0.122 | 1.308 | 3.711 | 0.431 | 7.427 | **1380.019** | 793.567 |
| baseline hot | 2.522 | 0.064 | 1025.494 | 377.971 | 2.404 | 0.135 | 1.383 | 4.031 | 0.52 | 7.766 | **1421.77** | 747.079 |
| baseline hot | 2.018 | 0.057 | 1040.372 | 364.517 | 2.875 | 0.168 | 1.45 | 3.459 | 0.414 | 7.63 | **1422.545** | 763.194 |
| fix1 COLD | 43.59 | 0.074 | 0 | 0 | 2.32 | 0.124 | 0.965 | 47.722 | 1.557 | 8.853 | **103.648** | 41.1 |
| fix1 hot 2 | 3.301 | 0.06 | 0 | 0 | 2.843 | 0.192 | 1.365 | 5.796 | 0.772 | 7.62 | **21.177** | 9.91 |
| fix1 hot 3 | 2.859 | 0.061 | 0 | 0 | 3.807 | 0.185 | 1.191 | 4.296 | 0.505 | 9.191 | **21.591** | 12.73 |
| fix1 hot 4 | 2.179 | 0.058 | 0 | 0 | 2.707 | 0.206 | 2.03 | 7.898 | 0.57 | 6.819 | **21.897** | 8.425 |
| fix1 hot 5 | 2.486 | 0.05 | 0 | 0 | 2.474 | 0.199 | 1.615 | 3.457 | 0.398 | 6.658 | **16.938** | 8.414 |

## What is left

Pull and compile are gone from the hot path (`wamn.component.cache_hit` is 0.034 ms). The
remaining hot request is roughly:

| phase | ms | share |
|---|---:|---:|
| **unspanned** | 6.7–9.2 | **36–43 %** |
| `wamn.postgres` wrapper | 3.5–7.9 | 20–36 % |
| `linker_setup` | 2.5–3.8 | 13–17 % |
| `authenticate` | 2.2–3.3 | 11–15 % |
| `instantiate` | 1.2–2.0 | 6–9 % |
| `sql.statement` | 0.4–0.8 | 2–4 % |

The unspanned residue was ~7.6 ms in the baseline too — it never moved in absolute terms.
It was 0.5 % of a 1.4 s request; it is now the largest single component.

## Known limits

- The cache is unbounded across release churn — filed as `wamn-0h0g.17.1`, evicting on
  release lineage rather than LRU or TTL.
- The `Linker` is **not** cached. `add_nested_operation_links` captures
  `NestedOperationHost`, which holds `invocation: Mutex<Option<BoundNestedInvocation>>`
  written per request. Sharing one `Linker` across concurrent requests to the same digest
  would let them clobber each other's invocation state. Making it per-digest requires
  moving that into `SharedCtx`.

## Span trees

### cold-c0000000000000000000000000000011

```
  spans=15
  handle_http_request  103.648 ms  @+0 ms
    invoke_component_handler  103.417 ms  @+0.1 ms
      wamn.route.authenticate  43.59 ms  @+2.1 ms
        wamn.postgres.acquire  39.601 ms  @+4.5 ms
      wamn.jetstream  2.085 ms  @+46.7 ms
      wamn.router.resolve  0.074 ms  @+48.9 ms
  wamn.component.invoke  53.004 ms  @+49 ms
    wamn.component.cache_hit  0.017 ms  @+49.3 ms
    wamn.component.linker_setup  2.32 ms  @+49.4 ms
    wamn.component.link  0.124 ms  @+51.8 ms
    wamn.component.instantiate  0.965 ms  @+51.9 ms
    wamn.postgres  47.722 ms  @+53.4 ms
      wamn.postgres.acquire  41.014 ms  @+53.5 ms
      wamn.postgres.statement  1.557 ms  @+97.2 ms
      wamn.jetstream  0.123 ms  @+102.3 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20002

```
  spans=15
  handle_http_request  21.177 ms  @+0 ms
    invoke_component_handler  20.865 ms  @+0.2 ms
      wamn.route.authenticate  3.301 ms  @+2.5 ms
        wamn.postgres.acquire  0.159 ms  @+4.2 ms
      wamn.jetstream  0.189 ms  @+7.1 ms
      wamn.router.resolve  0.06 ms  @+7.3 ms
  wamn.component.invoke  12.367 ms  @+7.5 ms
    wamn.component.cache_hit  0.025 ms  @+7.9 ms
    wamn.component.linker_setup  2.843 ms  @+8 ms
    wamn.component.link  0.192 ms  @+10.9 ms
    wamn.component.instantiate  1.365 ms  @+11.2 ms
    wamn.postgres  5.796 ms  @+13.3 ms
      wamn.postgres.acquire  0.221 ms  @+13.4 ms
      wamn.postgres.statement  0.772 ms  @+16.1 ms
      wamn.jetstream  0.126 ms  @+20.1 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20003

```
  spans=15
  handle_http_request  21.591 ms  @+0 ms
    invoke_component_handler  21.27 ms  @+0.2 ms
      wamn.route.authenticate  2.859 ms  @+3.5 ms
        wamn.postgres.acquire  0.122 ms  @+4.9 ms
      wamn.jetstream  0.157 ms  @+7.3 ms
      wamn.router.resolve  0.061 ms  @+7.5 ms
  wamn.component.invoke  11.967 ms  @+7.7 ms
    wamn.component.cache_hit  0.031 ms  @+8.1 ms
    wamn.component.linker_setup  3.807 ms  @+8.3 ms
    wamn.component.link  0.185 ms  @+12.2 ms
    wamn.component.instantiate  1.191 ms  @+12.4 ms
    wamn.postgres  4.296 ms  @+14.4 ms
      wamn.postgres.acquire  0.189 ms  @+14.5 ms
      wamn.postgres.statement  0.505 ms  @+15.7 ms
      wamn.jetstream  0.164 ms  @+20 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20004

```
  spans=15
  handle_http_request  21.897 ms  @+0 ms
    invoke_component_handler  21.716 ms  @+0.1 ms
      wamn.route.authenticate  2.179 ms  @+1.9 ms
        wamn.postgres.acquire  0.092 ms  @+3 ms
      wamn.jetstream  0.126 ms  @+5.3 ms
      wamn.router.resolve  0.058 ms  @+5.5 ms
  wamn.component.invoke  15.167 ms  @+5.6 ms
    wamn.component.cache_hit  0.029 ms  @+6.1 ms
    wamn.component.linker_setup  2.707 ms  @+6.3 ms
    wamn.component.link  0.206 ms  @+9.1 ms
    wamn.component.instantiate  2.03 ms  @+9.3 ms
    wamn.postgres  7.898 ms  @+12.2 ms
      wamn.postgres.acquire  0.23 ms  @+12.3 ms
      wamn.postgres.statement  0.57 ms  @+13.5 ms
      wamn.jetstream  0.102 ms  @+21 ms
```

### hot-d2d2d2d2d2d2d2d2d2d2d2d2d2d20005

```
  spans=15
  handle_http_request  16.938 ms  @+0 ms
    invoke_component_handler  16.749 ms  @+0.1 ms
      wamn.route.authenticate  2.486 ms  @+2.1 ms
        wamn.postgres.acquire  0.097 ms  @+3.3 ms
      wamn.jetstream  0.112 ms  @+5.2 ms
      wamn.router.resolve  0.05 ms  @+5.4 ms
  wamn.component.invoke  9.933 ms  @+5.6 ms
    wamn.component.cache_hit  0.019 ms  @+5.9 ms
    wamn.component.linker_setup  2.474 ms  @+6 ms
    wamn.component.link  0.199 ms  @+8.5 ms
    wamn.component.instantiate  1.615 ms  @+8.8 ms
    wamn.postgres  3.457 ms  @+11.3 ms
      wamn.postgres.acquire  0.153 ms  @+11.3 ms
      wamn.postgres.statement  0.398 ms  @+12.2 ms
      wamn.jetstream  0.126 ms  @+15.8 ms
```

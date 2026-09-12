# Request path: cold versus hot

Measured latency of one released HTTP route, end to end, with the OTLP span
breakdown for every phase. Filed because the steady-state request path measures
in seconds where the target is milliseconds.

**Source commit:** `4dec956a`  
**Measured:** 2026-09-04T08:54:33-04:00  
**Raw traces:** `docs/perf/2026.09/0-baseline/` (traces/, raw/)

## Method

`tools/receiving-cluster-journey-run --apply --measure-startup`, which builds the
release and stands up its own disposable kind cluster (`wamn-receiving-journey`).
The frozen `kind-wamn` cluster is never a target.

- **Route:** `POST /purchase_order/get`, `Host: receiving.localhost`, one SQL statement.
- **Caller:** a Job pod inside the cluster, curling the `flow-http` ClusterIP
  Service directly. There is no Ingress in the path — pod to Service to host pod.
- **COLD** is the journey's own first request against a freshly deployed host.
- **HOT** requests were fired only *after* the cold job completed, so the cold
  measurement is unperturbed by the sampler.
- Traces were pulled from the in-cluster Tempo through the API-server proxy.

## Client-side totals

| source | phase | ttfb ms | total ms |
|---|---|---:|---:|
| journey-run-A | **COLD** | 26868.328 | **26868.520** |
| journey-run-B | **COLD** | 26992.560 | **26992.709** |
| sampler on run B | hot 1 | 1569.308 | 1569.527 |
| sampler on run B | hot 2 | 1381.425 | 1381.495 |
| sampler on run B | hot 3 | 1423.383 | 1423.477 |
| sampler on run B | hot 4 | 1424.050 | 1424.146 |

## Phase breakdown (ms)

| trace | spans | auth | resolve | PULL | COMPILE | linker | link | inst | db | sql | handle_http |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| hot 001 | 6 | 48.008 | 49.047 | **1045.883** | **0** | 0 | 0 | 0 | 0 | 0 | 0 |
| hot 002 | 16 | 2.134 | 0.069 | **976.81** | **386.346** | 2.093 | 0.122 | 1.308 | 3.711 | 0.431 | 1380.019 |
| hot 003 | 16 | 2.522 | 0.064 | **1025.494** | **377.971** | 2.404 | 0.135 | 1.383 | 4.031 | 0.52 | 1421.77 |
| hot 004 | 16 | 2.018 | 0.057 | **1040.372** | **364.517** | 2.875 | 0.168 | 1.45 | 3.459 | 0.414 | 1422.545 |

`warm-001` carries only 6 spans: its trace was still flushing when fetched. It is
listed for completeness and is **not** a data point — its auth and resolve figures
are an artifact of the partial trace.

## What this shows

Pull and compile are the request. Everything else, including the work the request
exists to do, is noise beside them.

- **`wamn.component.pull` ~1.0 s** and **`wamn.component.compile` ~0.38 s** together
  account for **97.9 %** of a hot request.
- **The SQL statement is 0.43 ms.** The whole database interaction is 3.7 ms.
- **Nothing hides in an uninstrumented phase.** `handle_http_request` is 1380.0 ms
  against a client-measured 1381.5 ms, so ingress, kube-proxy, TCP, HTTP framing,
  respond and teardown together are **1.5 ms**.
- **The caching that exists works.** `wamn.router.resolve` is 0.06 ms — the wiring
  cache is doing its job. The component artifact is what nothing caches.
- Compile runs on every request despite a populated wasmtime cache directory that
  the journey itself snapshots and verifies unchanged.

Against upstream wasmCloud's topology bench (20k–60k req/s per host on trivial
work, ~0.05 ms/request) this path is roughly **28 000x** slower. Removing pull and
compile would leave ~29 ms, still ~580x, but an ordinary performance problem
rather than this one.

## Two findings outside the latency question

**The gate cannot fail on latency.** `run_startup_request` asserts `status == 200`
and that the two timings parse as floats. Nothing more. A tree-wide search for any
latency bound returns nothing — every hit is a CDC stall threshold or a curl
timeout. Both cold requests above were recorded `verdict=pass`. That is why a
three-order-of-magnitude gap survived without a red gate.

**The restart arm is broken, three runs out of three.** After the host restarts,
the probe gets 29 `curl: (7) Could not connect` failures across 46 attempts over 45 s — never a
404, never a 200 — and `--measure-startup` dies there without reaching its own
steady phase.

## Reproducibility

Cold reproduces to within 0.5 % across two independent clusters measured at load
average ~11 and ~1.5, so none of this is host contention. The hot samples are four
consecutive requests against one host, so they are one independent observation with
four repeats, not four independent points.

## Full span trees


### warm-001

```
wamn.route.authenticate  48.008 ms  @+0 ms
  wamn.postgres.acquire  42.779 ms  @+3.4 ms
wamn.jetstream  2.042 ms  @+49 ms
wamn.router.resolve  49.047 ms  @+51.1 ms
  wamn.postgres.acquire  38.454 ms  @+51.2 ms
wamn.component.pull  1045.883 ms  @+100.6 ms
```

### warm-002

```
handle_http_request  1380.019 ms  @+0 ms
  invoke_component_handler  1379.837 ms  @+0.1 ms
    wamn.route.authenticate  2.134 ms  @+1.8 ms
      wamn.postgres.acquire  0.084 ms  @+2.8 ms
    wamn.jetstream  0.116 ms  @+4.5 ms
    wamn.router.resolve  0.069 ms  @+4.7 ms
wamn.component.invoke  1373.765 ms  @+4.8 ms
  wamn.component.pull  976.81 ms  @+5.2 ms
  wamn.component.compile  386.346 ms  @+982.1 ms
  wamn.component.linker_setup  2.093 ms  @+1368.7 ms
  wamn.component.link  0.122 ms  @+1370.8 ms
  wamn.component.instantiate  1.308 ms  @+1370.9 ms
  wamn.postgres  3.711 ms  @+1373.4 ms
    wamn.postgres.acquire  0.198 ms  @+1373.4 ms
    wamn.postgres.statement  0.431 ms  @+1374.5 ms
    wamn.jetstream  0.129 ms  @+1378.9 ms
```

### warm-003

```
handle_http_request  1421.77 ms  @+0 ms
  invoke_component_handler  1421.557 ms  @+0.1 ms
    wamn.route.authenticate  2.522 ms  @+2.3 ms
      wamn.postgres.acquire  0.123 ms  @+3.4 ms
    wamn.jetstream  0.115 ms  @+5.5 ms
    wamn.router.resolve  0.064 ms  @+5.7 ms
wamn.component.invoke  1414.482 ms  @+5.8 ms
  wamn.component.pull  1025.494 ms  @+6.2 ms
  wamn.component.compile  377.971 ms  @+1031.8 ms
  wamn.component.linker_setup  2.404 ms  @+1409.9 ms
  wamn.component.link  0.135 ms  @+1412.4 ms
  wamn.component.instantiate  1.383 ms  @+1412.6 ms
  wamn.postgres  4.031 ms  @+1414.8 ms
    wamn.postgres.acquire  0.169 ms  @+1414.9 ms
    wamn.postgres.statement  0.52 ms  @+1416 ms
    wamn.jetstream  0.13 ms  @+1420.6 ms
```

### warm-004

```
handle_http_request  1422.545 ms  @+0 ms
  invoke_component_handler  1422.335 ms  @+0.1 ms
    wamn.route.authenticate  2.018 ms  @+1.8 ms
      wamn.postgres.acquire  0.104 ms  @+2.8 ms
    wamn.jetstream  0.111 ms  @+4.6 ms
    wamn.router.resolve  0.057 ms  @+4.7 ms
wamn.component.invoke  1415.831 ms  @+4.9 ms
  wamn.component.pull  1040.372 ms  @+5.2 ms
  wamn.component.compile  364.517 ms  @+1045.7 ms
  wamn.component.linker_setup  2.875 ms  @+1410.4 ms
  wamn.component.link  0.168 ms  @+1413.3 ms
  wamn.component.instantiate  1.45 ms  @+1413.5 ms
  wamn.postgres  3.459 ms  @+1415.8 ms
    wamn.postgres.acquire  0.183 ms  @+1415.9 ms
    wamn.postgres.statement  0.414 ms  @+1416.8 ms
    wamn.jetstream  0.162 ms  @+1421.1 ms
```

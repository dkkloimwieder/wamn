# Fix 5 — name the residue: spans around each host call on the p2 path

**Source commit:** `b435b974` on main (fork pin `2eadd937`, patch `wamn-2w3x.3`; authored on `perf/residue-spans` off `a80ef5f1`)  
**Measured:** 2026-09-05T17:24:37-04:00  
**Load average at launch:** 7.71, 9.25, 8.97 (a teammate's cold lane test build overlapped the image build; 3.76 by the end of the measurement)  
**Data:** `docs/perf/2026.09/5-residue-spans/`  
**Bead:** `wamn-0h0g.17.16`

Instrumentation only — no behaviour changed. After `1c-c` a third of a hot request,
4–5 ms, carried no span, and the p3 probe showed the same share under
`wasi:http@0.3`, so it was not the p2 body path's to give back. Every unnamed gap sat
inside `invoke_component_handler`, which is fork code: the flow guest's store build
and instantiate, the head hand-off and the teardown. Nine `info_span`s in the fork's
`host/http.rs` now name the host's own calls there, so the guest's path shows as the
gaps between them.

## Spans added (fork `2eadd937`, `crates/wash-runtime/src/host/http.rs`)

| span | site | hot avg (4 req) |
|---|---|---:|
| `http.route` | `handler.route_incoming_request` — the wamn router plugin's match | 0.037 ms |
| `http.lookup_service` | `service_handlers.read()` | 0.025 ms |
| `http.lookup_workload` | `workload_handles.read()` | 0.019 ms |
| `http.new_store` | `workload_handle.new_store` — the per-request `Store<SharedCtx>` from the ctx templates | 0.267 ms |
| `http.incoming_request` | `new_incoming_request` + `new_response_outparam` + `ProxyPre::new` | 0.069 ms |
| `http.instantiate` | `pre.instantiate_async` — the flow-http guest's instantiate | **0.685 ms** |
| `http.handle` | `call_handle` — the guest's whole run; a container for every in-tree span | 10.833 ms |
| `http.await_head` | the request side waiting for `response-outparam.set` — a waiter, not work | 11.911 ms |
| `http.store_drop` | explicit `drop(store)` in the detached task | 0.142 ms |

## The timeline (hot 3, `trace-5-hot-…0003.json`)

```
handle_http_request                    @   0.000 + 13.435
  http.route                             @   0.064 +  0.036
  http.lookup_service                    @   0.149 +  0.023
  http.lookup_workload                   @   0.191 +  0.018
  invoke_component_handler               @   0.260 + 13.074
    http.new_store                         @   0.298 +  0.288
    http.incoming_request                  @   0.632 +  0.065
    http.await_head                        @   0.740 + 12.558
    http.instantiate                       @   0.798 +  0.666
    http.handle                            @   1.506 + 11.529
      wamn.route.match                       @   2.006 +  0.082
      wamn.route.authenticate                @   2.554 +  2.585
        wamn.auth.pat                          @   2.591 +  0.591
        wamn.auth.roles                        @   3.233 +  0.358
        wamn.auth.permissions                  @   3.636 +  1.448
          wamn.postgres.acquire                  @   3.680 +  0.107
          wamn.auth.perm.query                   @   3.827 +  0.461
      wamn.route.validate_input              @   5.424 +  0.305
      wamn.route.permit                      @   5.818 +  0.032
      wamn.jetstream                         @   6.114 +  0.110
      wamn.router.resolve                    @   6.267 +  0.057
wamn.component.invoke                  @   6.426 +  5.681
  wamn.component.cache_hit               @   6.822 +  0.025
  wamn.component.link                    @   6.959 +  0.199
  wamn.component.linker_setup            @   7.194 +  0.718
    wamn.linker.hosts                      @   7.217 +  0.020
    wamn.linker.register                   @   7.249 +  0.107
    wamn.linker.scope                      @   7.432 +  0.364
      wamn.linker.revokes                    @   7.458 +  0.035
      wamn.linker.pending_scope              @   7.522 +  0.115
        wamn.scope.clear                       @   7.545 +  0.014
        wamn.scope.bind                        @   7.580 +  0.042
      wamn.linker.ctx                        @   7.654 +  0.138
  wamn.component.instantiate             @   7.932 +  1.253
  wamn.postgres                          @   9.868 +  1.616
    wamn.postgres.acquire                  @   9.948 +  0.167
    wamn.postgres.session_settings         @  10.158 +  0.603
    wamn.postgres.statement                @  10.818 +  0.635
      wamn.postgres.decode_rows              @  11.308 +  0.098
      wamn.jetstream                         @  12.345 +  0.122
    http.store_drop                        @  13.071 +  0.134
```

## Who owns each millisecond

`tools/phases.py` over the four hot requests. The fork's spans are the boundaries:
what is outside `http.handle` is the host's, what is inside and under no in-tree span
is the guest's own path (its compute, the `wasi:http` import implementations it calls
into, and the bindings marshalling between them).

| ms per request | hot 2 | hot 3 | hot 4 | hot 5 | **avg** | share |
|---|---:|---:|---:|---:|---:|---:|
| total (`handle_http_request`) | 13.584 | 13.435 | 12.498 | 11.539 | **12.764** | 100 % |
| fork host calls (the seven leaves above) | 1.184 | 1.229 | 1.223 | 1.337 | **1.243** | 10 % |
| fork glue between them (spawn, span entry, head wake-up, `watch_body`) | 0.713 | 0.677 | 0.680 | 0.681 | **0.688** | 5 % |
| **flow-http guest's own path** (`http.handle` minus every in-tree span) | 2.642 | 2.678 | 3.242 | 2.535 | **2.774** | **22 %** |
| route plugin spans (match, validate, permit, resolve, jetstream) | 0.428 | 0.586 | 0.358 | 0.471 | **0.461** | 4 % |
| authenticate, named children (pat, roles, permissions) | 1.797 | 2.397 | 1.665 | 1.633 | **1.873** | 15 % |
| authenticate, its own | 0.185 | 0.188 | 0.217 | 0.186 | **0.194** | 2 % |
| `wamn.component.invoke`, named children | 4.591 | 3.812 | 3.450 | 3.058 | **3.728** | 29 % |
| `wamn.component.invoke`, its own (driver + the data-access guest's own path) | 2.043 | 1.869 | 1.663 | 1.639 | **1.804** | 14 % |

The rows sum to the total on every trace (the script asserts it).

## What the residue was

The bead carried two hypotheses from `3a`. Both are refuted by name.

**"~1.8 ms before `wamn.route.match` is the request body read."** No. From
`handle_http_request` to the guest's first host call (hot 3): 0.06 before `http.route`,
route and the two lookups 0.08, 0.07 to `invoke_component_handler`, `http.new_store`
0.29, `http.incoming_request` 0.07, 0.12 to spawn the task, **`http.instantiate` 0.67**,
then **0.50 of the guest's own start** before it calls `routes`. The body is not read
here at all: the guest reads it between `authenticate` and `validate_input`, and that
gap is 0.3 ms. The named cause is the flow-http guest's per-request instantiate plus
its store build plus its own start — 1.5 ms of a fresh p2 store, not I/O.

**"~1.4 ms after the last in-tree span is response serialization."** Mostly not the
host's. After the data-access side's last span: **0.57 ms of the guest building and
setting its response** (its own path), then `http.store_drop` 0.13, 0.09 for the request
side to wake on the head, 0.14 for `watch_body` and the return. The fork's own tail is
about 0.35 ms; the rest is the guest.

**The finding: the residue is the guests' own paths.** The host's newly named calls
total 1.24 ms, its glue 0.69 — 15 % of the request, of which the flow guest's
`http.instantiate` (0.69) and `http.new_store` (0.27) are the only items over 0.15 ms.
The flow-http guest's own path, **2.77 ms, is now the largest unnamed item in the
request**, and the data-access guest's own path sits inside the driver's 1.80 ms
(0.70 between `instantiate` and `postgres.acquire`, 0.90 after the statement). Together
the two guests own about 4.6 ms, 36 % of a hot request — the share that did not move
under p3, because it never was the p2 body path.

Smaller named items worth a line:

- `wamn.auth.permissions` carries 0.14–0.22 ms of its own after `perm.query`, and
  0.88 ms on hot 3 — an outlier, one trace in four.
- The driver spends 0.47 ms between `wamn.component.invoke` starting and
  `wamn.component.cache_hit` — in-tree, unspanned.
- `http.await_head` ends 0.09–0.26 ms after `http.handle`: the head reaches the
  request side only after the guest's task has returned and dropped its store. The
  guest sets the out-param at the end of its run, so no streaming is lost, but the
  wake-up is serialized behind the teardown.

## Scope, from the evidence

Nothing here is a refactor this lane should choose. The candidates, priced, are the
owner's call on `wamn-0h0g.17.28`:

1. **Split the flow guest's 2.77 ms.** A guest-side monotonic-clock probe
   (`wasi:clocks` reads at the guest's own boundaries, emitted as a response header)
   separates compute from time inside `wasi:http` imports, which live in
   `wasmtime-wasi-http`, not the fork, and run in the debug-built host. Cheap, in-tree,
   the next instrumentation step if the guest path is to be scoped at all.
2. **The fresh p2 store: 1.1 ms** (`new_store` 0.27 + `instantiate` 0.69 +
   `store_drop` 0.14). The fork's `InstancePool` removes all three, and ledger row 4
   keeps reuse off by ruling (fresh store per invocation). A policy decision, not a
   patch.
3. **Driver gaps, 0.47 + 1.80 ms.** Spans in `router_driver.rs` around the invoke
   entry and the data-access call's return path — the same cheap step as this lane,
   in-tree.
4. `wamn.auth.permissions`' own 0.14–0.88 ms — in-tree, a span first.

## Overhead ratio — red on the steady probe

The gate's single steady request read **10.55 against the ceiling 9**:
`handle_http_request` 17.554 ms over `wamn.postgres.statement` 0.419 +
`wamn.component.instantiate` 1.245. The four hot requests of the same run, same host,
same minute, read **5.59, 7.12, 7.00, 7.49**. The probe's decomposition shows a slow
moment, not a different shape: its fork host calls 2.02 ms against 1.23, its flow guest
path 5.45 against 2.68, everything 1.5–2× — and a fast statement in the denominator.
The ceiling is not relaxed here. The question raised for the owner is the gate's
sample of one (`wamn-0h0g.17.29`).

Other receipts: cold 104.7 ms, restart-first 123.6 ms with 75 s of recovery (37–39 s on
the previous runs, under the same overlapping build), steady 21.1 ms at the client.

## Method

Journey `tools/receiving-cluster-journey-run --apply --measure-startup` from the lane at
`b435b974`; one cold, four hot and one write request from an in-cluster probe pod
(`5-residue-spans/measure.log`, `client-5.log`); traces pulled from Tempo by trace id
(`trace-5-*.json`). Attribution by `docs/perf/2026.09/tools/gaps.py` (every gap between
consecutive named spans, labelled by its neighbours) and `tools/phases.py` (the owner
table above; containers are not coverage, every other span is a named leaf, and the
decomposition must sum to the total). Both scripts take trace files as arguments.

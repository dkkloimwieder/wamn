# Request-path performance, 2026.09

One directory per increment. Each `<n>-<fix>.md` is the report; the matching
`<n>-<fix>/` directory holds its raw traces and evidence, so every number in a
report regenerates from data committed beside it.

| report | data | subject | status |
|---|---|---|---|
| `cold-v-hot.md` | `0-baseline/` | baseline: where a request's time goes | done |
| `1-component-cache.md` | `1-component-cache/` | compiled `Component` cached in-process by digest, preloaded at schedule | done |
| `3a-instrument.md` | `3a-instrument/` | span every unmeasured gap — instrumentation only | done |
| `3b-pipeline.md` | `3b-pipeline/` | claim transaction and statement in one flight | done |
| `3c-operation-kind.md` | `3c-operation-kind/` | PostgreSQL decides which statements need a transaction | done |
| `2a-auth-instrument.md` | `2a-auth-instrument/` | span the three authenticate legs — instrumentation only | done |
| `2-auth.md` | `2-auth/` | collapse the auth round trips: nine to three | done |
| `1b-a-linker-instrument.md` | `1b-a-linker-instrument/` | split `linker_setup` — instrumentation only | done |
| `1b-linker-clone.md` | `1b-linker-clone/` | link the WASI surface once, clone it per request | done |
| `4-instantiate.md` | `4-instantiate/`, `4a-release-profile/` | the served guest was a debug build; instantiate is not size-driven | done |
| `1c-residue-and-scope.md` | `1c-a-residue/`, `1c-scope-split/` | what `linker_setup` is made of — instrumentation only | done |
| `1c-instance-pre.md` | `1c-instance-pre/` | un-fuse registration from linker population, seal one `InstancePre` per digest | done |
| `1c-b-scope-split.md` | `1c-b-scope-split/` | inside `pending_scope`: the per-request statement digest hash — instrumentation only | done |
| `1c-c-statement-sets.md` | `1c-c-statement-sets/` | statement sets verified and lowered once per digest; `linker_setup` under 1 ms | done |
| `p3-probe.md` | `p3-probe/` | probe: `wasi:http@0.3` serves the route; the unspanned residue's share does not move | done |
| `5-residue-spans.md` | `5-residue-spans/` | spans around each host call on the p2 path (fork `2eadd937`): the residue is the guests' own paths — instrumentation only | done |
| `6-throughput.md` | `6-throughput/` | concurrency 1–64 across the route, a no-DB route and the direct statement: the host's 2-core quota is what saturates first, at about 150 req/s | done |
| `7-release-host.md` | `7-release-host/` | the host as a release build on the default 6-core cap: 4.8 ms in-host, 1,650 req/s at the knee, the guest SQL pool is what saturates next | done |
| `7a-after-async-handler.md` | `7a-after-async-handler/` | the same sweep after `wamn:node/async-handler` landed: the sync hot path did not move; the mid-sweep drop is the box's | done |

**Read the load average in each report before comparing absolute milliseconds
across them.** The journey builds on the same machine it measures, so totals
move with load. The overhead ratio and within-trace proportions do not.

Each report carries source commit, machine load average at measurement, the
phase table before and after, the overhead ratio, and span trees verbatim.

**Overhead ratio** is the load-independent gate: hot `handle_http_request`
divided by the sum of `wamn.postgres.statement` + `wamn.component.instantiate`.
It bounds platform overhead against real work, so it does not move when the
dev machine is busy. No test asserts an absolute latency.

**The gate now executes.** As of `1b-linker-clone` the journey passes end to
end and emits `verdict=pass phase=steady overhead_ratio=8.659 ceiling=12`. It had
never run before: the restart arm failed ahead of it for seven consecutive
journeys. The ceiling ratchets down as the remaining fixes land and is never
relaxed.

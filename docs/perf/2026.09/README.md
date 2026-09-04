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
| `2-auth-cache.md` | | auth resolution cached per PAT hash | |
| `1b-linker-cache.md` | | move invocation state to `SharedCtx`, cache the `Linker` | |
| `4-instance-pool.md` | | warm instance pool keyed `(tenant, digest)` | |

**Read the load average in each report before comparing absolute milliseconds
across them.** The journey builds on the same machine it measures, so totals
move with load. The overhead ratio and within-trace proportions do not.

Each report carries source commit, machine load average at measurement, the
phase table before and after, the overhead ratio, and span trees verbatim.

**Overhead ratio** is the load-independent gate: hot `handle_http_request`
divided by the sum of `wamn.postgres.statement` + `wamn.component.instantiate`.
It bounds platform overhead against real work, so it does not move when the
dev machine is busy. No test asserts an absolute latency.

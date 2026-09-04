# Request-path performance, 2026.09

One directory per increment. Each `<n>-<fix>.md` is the report; the matching
`<n>-<fix>/` directory holds its raw traces and evidence, so every number in a
report regenerates from data committed beside it.

| report | data | subject |
|---|---|---|
| `cold-v-hot.md` | `0-baseline/` | baseline: where a request's time goes |
| `1-component-cache.md` | `1-component-cache/` | compiled `Component` cached in-process by digest |
| `2-auth-cache.md` | `2-auth-cache/` | auth resolution cached per PAT hash |
| `3-postgres-wrapper.md` | `3-postgres-wrapper/` | the wrapper around a 0.4 ms statement; spans and causation off the request path |
| `4-instance-pool.md` | `4-instance-pool/` | warm instance pool keyed `(tenant, digest)` |

Each report carries source commit, machine load average at measurement, the
phase table before and after, the overhead ratio, and span trees verbatim.

**Overhead ratio** is the load-independent gate: hot `handle_http_request`
divided by the sum of `wamn.postgres.statement` + `wamn.component.instantiate`.
It bounds platform overhead against real work, so it does not move when the
dev machine is busy. No test asserts an absolute latency.

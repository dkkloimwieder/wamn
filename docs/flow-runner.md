# Flow-runner (5.2)

The production flow-runner executes a [`wamn-flow`](flow-schema.md) graph: it
walks the nodes from `entry` following ported edges, branches and merges, routes
errors, and retries with backoff. It replaces the S3 spike's ad-hoc linear walk.

The design follows the same split as the API gateway (4.1): a **pure engine
crate** (`crates/wamn-runner`) holds all the decision logic and is unit-tested
with no cluster, no DB, no wasm; a **thin component** (`components/flowrunner`)
supplies the effects — dispatching each node, the `wamn:postgres` checkpoints,
the reload doorbell.

## The engine — a pure reducer

`wamn-runner` is a synchronous state machine. The driver loop is:

```rust
let mut st = plan.start(run_id, input);
loop {
    match plan.next(&mut st, now_ms()) {
        Step::Dispatch(d)  => { let o = run_node(&d); plan.apply(&mut st, &d, o, now_ms()); }
        Step::Wait { until_ms, throttle, .. } => { /* gate `throttle`, sleep to until_ms */ }
        Step::Done(status) => break status,
    }
}
```

`Plan::compile(&flow)` validates the flow (`wamn_flow::validate`) and indexes it.
`next` decides the next `Step`; `apply` folds a node's `NodeOutcome` into the run.
Every effect is the driver's — the engine holds no clock, DB, host, or wasm, which
is what makes the whole thing testable in-process.

### Walk model (v1)

A single-token BFS frontier. A node emits on a **port**; the engine enqueues the
edges leaving that port.

- **Branch** = a node emits on one of several ports (a `conditional` selecting
  `"true"`/`"false"`). Only the selected port's edges are followed.
- **Merge** = several edges into one node. There is no join *barrier* in v1: a
  merged node runs once per arriving token (join barriers are a later item).
- **Fan-out** (several edges from one port) runs sequentially in frontier order
  (true per-node parallelism is 5.11).
- **Cycles** are permitted by the schema; loop termination is the node/config's
  concern (the engine does not force acyclicity).

## Error taxonomy → runner action (mechanical, no string-matching)

A dispatched node returns `NodeOutcome::Success { payload, port }` or
`NodeOutcome::Error(NodeError)`. `NodeError` mirrors the `wamn:node` `node-error`
WIT variant (`docs/wamn-node.wit`); the engine's action for each is fixed:

| `NodeError`      | Runner action |
|------------------|---------------|
| `Retryable`      | Retry per the node's [`RetryPolicy`](#retry-policy); on exhaustion, error-path or fail. |
| `RateLimited`    | Retry honoring the source `retry-after` (else the backoff curve) **and** engage the shared throttle. |
| `Terminal`       | Route to the flow's `error` port immediately; if none, fail (`FailKind::Terminal`). |
| `InvalidInput`   | **Never** retried; error-path or fail (`FailKind::InvalidInput`) — flagged distinctly for run history. |
| `Cancelled`      | Run recorded `Cancelled`; error branches do **not** fire. |

Routing keys off the *variant*, never a message string. A `Terminal`/`InvalidInput`
node with a `error`-port edge continues down that branch (the error node receives
`{"error": {message, code, data}}`); with no error edge, the run fails.

### Retry policy

Per-node, read from a reserved `"retry"` object in the node's opaque `config`
(`max-attempts` / `base-ms` / `factor` / `cap-ms`), defaulting to 3 attempts,
100 ms base, ×2, capped at 30 s. Backoff is **deterministic** exponential
(`min(cap, base·factorⁿ)`) — no jitter — so the engine stays pure; a driver may
jitter around the returned delay. `attempt` (0-based) and a stable
`idempotency-key` (`{run_id}:{node}`) are threaded to each dispatch, matching the
`wamn:node` `run-context`.

### Shared throttle + concurrency (cross-run)

Two cross-run coordination structures live beside the reducer (`throttle.rs`),
both pure (time is a `now_ms` argument):

- **`ThrottleTable`** keyed by `ThrottleKey { node_type, credential, host }`: when a
  node returns `rate-limited`, every parallel execution against the *same* limited
  system backs off together, while unrelated flows proceed. This is **not** run-queue
  backpressure (that is 5.14) — one throttled upstream must not stall the platform.
- **`Scheduler`**: the per-flow in-flight cap; `try_admit` refuses a flow's run past
  its limit (claim-side backpressure — the durable queue is 5.14).

## The component — driving the engine

`components/flowrunner` loads the active flow (`SELECT graph_json FROM flows …`,
now a `wamn-flow` document), compiles a `Plan`, and drives it. The native standard
nodes are the `NodeOutcome` producers: `webhook-in` / `transform` / `conditional` /
`respond` are pure same-binary calls; `pg-write` writes to the sink; `delay` reads
wall-clock and parks; `http-call` makes a `wasi:http` request. `wamn:postgres` is
the only durable path, under a host-injected tenant claim + `search_path`.

### Checkpoint / resume

The engine re-walks from `entry` on every invocation; the DB `step_seq` (the node's
index in the flow) is the checkpoint. An effectful node whose index is `<= step_seq`
skips its effect on replay; `pg-write` is additionally idempotent by `(run_id, step)`,
so a crash in the window between its commit and its checkpoint replays cleanly
(exactly-once effect). `delay` parks by recording a wake deadline in `state_json`
without advancing `step_seq`, so a later invocation re-enters it — the durable
parked-wake the S6 24-hour-delay test exercises under virtual time. Branch-aware
durable resume (persisting the frontier) is 5.7; the linear fixture flows resume
exactly on `step_seq`.

## Scope (5.2) vs. siblings

5.2 owns the single-runner execution engine. It deliberately does **not** own:

| Concern | Owner |
|---|---|
| The `node-error` taxonomy + SDK | `wamn-node-sdk` (5.3, ahead of the 5.4 WIT freeze; re-exported here as `NodeError`) |
| Durable `runs`/`node_runs` schema, at-least-once, branch-aware replay | 5.7 |
| Per-node ordering (`strict`/`partitioned`/`unordered`) | 5.11 |
| The `cancel(run, reason)` operation + its two enforcement layers | 5.12 |
| The durable run queue (`FOR UPDATE SKIP LOCKED`) + NATS doorbell + dispatcher | 5.14 |
| Payload store & byte quotas | 5.10 |
| The standard node *contents* (incl. the Postgres/raw-SQL node, D8) | 5.3 — SHIPPED: `wamn-nodes` (docs/node-library.md) |
| The custom-node HTTP transport | 5.6 |

The doorbell here is the consumer side only (recompile the `Plan` on a new active
version); the sync-webhook path (D15) surfaces as the runner's request/response
entry point, not the trigger front-end.

## Gates

- **`cargo test -p wamn-runner`** — the whole engine: linear/branch/merge/fan-out
  walk, error-path routing, retry-then-succeed / retry-exhausted, rate-limited
  `retry-after` + throttle key, invalid-input never-retried, cancelled, the retry
  policy, the throttle table, and the scheduler — all with no cluster.
- **`flowbench`** (S3) + **`testhostbench`** (S6) are the component's regression,
  unchanged: dispatch p99 < 50 µs, hot-reload < 1 s, kill-mid-run exactly-once, and
  the S6 sameness / 24 h-delay-under-virtual-time / egress-spy gates. Both pass on
  the engine-driven runner in-cluster (the gate of record) and locally.

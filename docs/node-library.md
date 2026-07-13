# Standard node library v1 (5.3)

Two crates deliver plan item 5.3 (wamn-3xa):

| Crate | What it is |
|---|---|
| `crates/wamn-node-sdk` | The node **authoring contract** — the `Node` trait, the `RunContext` view of a dispatch, the `NodeCtx` capability facade every effect flows through, and the `wamn:node` error taxonomy (`NodeError`/`ErrorDetail`/`RateLimitDetail`, now DEFINED here and re-exported by `wamn-runner`). A Rust mirror of the drafted `docs/wamn-node.wit`; the 5.4 freeze layers the WIT + guest scaffolding on top. |
| `crates/wamn-nodes` | The **standard library**: the production node vocabulary plus the dispatch-time capability policy table. Pure — no DB, no wasm, no host; a mock `NodeCtx` unit-tests every node, classification map, and policy negative. |

`components/flowrunner` adopts the library: any node type `wamn-nodes` ships
dispatches through `wamn_nodes::dispatch` over a `NodeCtx` implemented on the
component's real imports (`wamn:postgres`, `wasi:http`). The S3/S6 fixture
node shapes keep their legacy semantics byte-identical (a `transform` /
`conditional` with an `expression` config routes to the library; the fixture
`op`/`min-len` shapes do not), so flowbench/testhostbench/f1bench regress
unchanged — all three re-ran green in-cluster on the adopted guest.

## The purity rule (5.13), enforced mechanically

Standard node crates depend on the SDK crate ONLY — never the runner — so no
node can circumvent the `wamn:node` interface and silently break the
frozen-flow composition path. `wamn-runner` depends on `wamn-node-sdk` (one
taxonomy definition, dependency pointing SDK-ward), never the reverse.

Enforcement is `crates/wamn-nodes/tests/purity.rs`: it walks `cargo metadata`'s
resolved NORMAL dependency edges and fails if `wamn-runner` (or any
host/store-side crate) enters the closure, or if the direct-dependency set
drifts from the declared allowlist (`wamn-node-sdk`, `wamn-api`, `serde_json`,
`jmespath`). Adding a dependency is a conscious, test-updating act.
Mutation-verified: adding `wamn-runner` to `wamn-nodes` fails both tests.

## Expressions: JMESPath, off the shelf

`transform`, `conditional`, `{{...}}` templating, and the Postgres nodes' value
selection all use **JMESPath** (the `jmespath` crate) — a frozen public spec,
so there is no language of our own to maintain (user decision, wamn-3xa).
Properties that made it the fit:

- JSON → JSON, side-effect free, **no arithmetic operators** — it can select,
  reshape (multiselect hashes construct objects), compare, and filter, but it
  cannot manufacture floats out of the exact-decimal STRINGS catalog numerics
  travel as. The no-float rule holds through a transform *by construction*.
- Numbers ride `serde_json::Number` exactly (pinned by test: 2^53+1 survives).
- A missing path yields `null`, not an error; a malformed expression fails
  compile → `terminal("invalid-expression")`.

Expressions compile per dispatch; memoizing per (flow-version, node-id) is the
note-9b refinement if profiles ever demand it.

## The vocabulary

| type | capabilities | config | emits |
|---|---|---|---|
| `transform` | — | `{"expression"}` | the expression's result on `main` |
| `conditional` | — | `{"expression"}` | input unchanged on `"true"`/`"false"` by JMESPath truthiness (`0` is truthy; `[]`/`""`/`{}`/`null`/`false` falsy) |
| `http-request` | `HttpEgress` | `{"method"?, "url" (templated), "headers"? (values templated), "body"? (jmespath; null ⇒ no body, else JSON)}` | `{"status", "headers", "body"}` on `main` |
| `postgres` | `Postgres` | `{"entity", "op": create\|get\|update\|delete\|list, "id"? (jmespath, default `id`), "body"? (jmespath, default `@`; managed `id`/`tenant_id` stripped), "filters"?/"sort"?/"limit"?/"offset"?}` | the row / row array / `{"deleted", "id"}` |
| `postgres-query` | `Postgres` + `RawSql` | `{"sql", "params"?: [jmespath per `$n`], "mode": query\|execute}` | `{"rows": [...]}` / `{"rows-affected": n}` |
| `respond` | — | `{"status"?}` | input unchanged on `main`; the DRIVER answers with it, reading the status via the pure `respond::status_for` |

Runner-intrinsic (documented here, not trait-dispatched): `delay` (parking is
an engine/driver concern — the durable parked-wake via `runs.state_json`) and
the trigger entry (`webhook-in` in the fixtures; production entry payloads come
from the trigger).

**Deliberately absent in v1** (scope decisions, wamn-3xa):

- **Loop/split/merge nodes** — loops are STRUCTURAL: cycles + `conditional`
  express them (the 5.1 schema allows cycles; the engine walks them). Dedicated
  split/merge nodes land with the 5.11 ordering/concurrency semantics
  (wamn-1d4) — the current walk is single-token BFS with no join barrier, so a
  parallel split/join would be a lie.
- **email/notify** — no email egress capability exists; notify is an
  `http-request` in disguise. Follow-up bead filed.

## The Postgres nodes (D8, wamn-r13)

**`postgres`** (entity ops) is the UNFLAGGED default: ops compile through the
SAME audited surface the generated REST gateway uses — `wamn_api::Router`
(4.1): identifiers catalog-allowlisted + quoted, values ALWAYS `$n` params,
`tenant_id` on create injected server-side, RLS floor underneath. The catalog
comes from the project's published `wamn_catalog` snapshot (`NodeCtx::
catalog_json`, the same document the api-gateway reads). Row shaping is
`wamn_api::shape_rows` — numerics come back as exact-decimal STRINGS.

**`postgres-query`** (raw SQL) declares `RawSql`, which the runner grants only
when the project's D8 flag is ON — **default OFF**: the dispatch check refuses
the node with `terminal("capability-denied")` (naming the flag) before it
runs, and nothing reaches the database. Values still bind as `$n` params (a
`params` array of JMESPath expressions over the input; strings travel as text
the server casts per column type — exact decimals stay exact). Enablement for
real projects is gated on the dedicated user-SQL role (wamn-1nd); the
flowrunner's facade hard-returns `raw_sql_enabled() = false` until that lands.

## The capability policy table

`Node::capabilities()` is the row; `wamn_nodes::dispatch` enforces it twice:

1. **Grant check** — the declared row must be covered by what the runner
   granted (`granted_for(raw_sql_enabled)`), else
   `terminal("capability-denied")` *before the node runs*.
2. **Gated facade** — the node receives a `NodeCtx` NARROWED to its declared
   row; an undeclared call fails with `NotGranted` even if the implementation
   is buggy.

Both layers are pure and mutation-tested (neutered grant check and allow-all
facade each fail named tests).

## Mechanical taxonomy classification

Nodes never string-match; the maps are fixed and unit-pinned:

- **Postgres** (per the frozen `wamn:postgres` 0.1 WIT annotation):
  serialization-failure / connection-unavailable / statement-timeout →
  `retryable`; constraint violations (carrying the constraint name in
  `data`), permission-denied, row-limit, query-error → `terminal`.
  `wamn_api` compile refusals split by fault: `invalid-value` /
  `payload-required` → `invalid-input` (the caller's data, never retried);
  everything else → `terminal` (a flow/config bug).
- **HTTP**: 429 → `rate-limited` (integer `Retry-After` honored as the source
  delay; `target_host` = the URL authority keys the shared throttle); 408/5xx
  → `retryable`; other 4xx → `terminal`; transport failure → `retryable`; a
  host egress denial (`allowedHosts`) → `terminal`.

## Driver notes (components/flowrunner)

- **Error rows**: the driver records a `node_runs` error row ONLY when the
  engine will ROUTE the emission — an error edge exists AND the variant/
  attempt says no retry follows (`will_error_route` mirrors the exact
  `RetryPolicy` computation `Plan::apply` makes). A row for a retried or
  edge-less failure would make 5.7 reconstruction resume the run down a path
  the live walk never took. Run failures land in `runs.fail_*` (audit parity
  with poc-webhook-f1).
- **Retry waits**: a scheduled retry surfaces as `Step::Wait`; this
  per-invocation driver treats it defensively as a failed run (poc-webhook-f1's
  sync rule). Cross-invocation retry scheduling belongs to the queue layer
  (`run_queue.available_at` / `park_sql`) and lands with the guest-claim
  rewire (wamn-fqg.4).
- The SDK `Emission` port is now IN the frozen contract: the 5.4 freeze
  amended `run` to return an emission record `{payload, port: option<string>}`
  (absent = `main`) before freezing 0.1 — WIT and SDK coincide, drift-guarded
  by `crates/wamn-node-sdk/tests/wit_coherence.rs`. Remaining SDK-side
  deferrals: `streamed` payloads (5.10) and the credentials facade (5.9).

## Gates

- `cargo test -p wamn-nodes` — every node against a mock facade (behavior,
  config negatives, both taxonomy maps, policy negatives, injection witnesses
  for both Postgres nodes, JMESPath number pinning) + the purity lint.
- `cargo test -p wamn-runner` — engine regression + the SDK port-constant
  drift-guard.
- flowbench (S3) + testhostbench (S6) + f1bench — the adopted guest regresses
  the fixture flows unchanged (in-cluster gates of record re-run PASS).
- Mutants killed (apply/test/restore): neutered grant check, allow-all gated
  facade, pg taxonomy swap (connection-unavailable → terminal), http taxonomy
  swap (5xx → terminal), runner dep added to wamn-nodes — each fails named
  tests.

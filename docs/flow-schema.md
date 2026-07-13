# Flow Graph Schema (5.1) — canonical `0.1`

The canonical, versioned representation of a dataflow. **A flow is data, not
code**: a directed graph of typed nodes wired by ported edges, invoked by one
trigger, referencing credentials by name. Deploying a flow flips an
active-version pointer + a NATS doorbell (5.14); the graph never becomes a
build artifact (the frozen-flow backend, 5.13, is a separate opt-in path — D1).

- **Issue:** wamn-34t `[5.1]`; **Epic:** E5 Dataflow Engine.
- **Contract file:** [`flow-schema.schema.json`](flow-schema.schema.json) — the
  language-neutral JSON Schema, **generated** from the Rust types (single source
  of truth) and drift-guarded by a test.
- **Crate:** `crates/wamn-flow` — types, import/export, validation, version diff.
- **Consumers:** the production runner (5.2), node library (5.3), editor (5.8),
  credential vault (5.9), dispatcher (5.14).

## Model

A `Flow` is **one version** of a flow (the unit stored in the catalog):

| Field | Type | Notes |
|---|---|---|
| `schema-version` | string | Flow-schema **format** version, e.g. `"0.1"`. Distinct from `version`. |
| `flow-id` | string | Stable across every version of this flow. Lowercase slug: `[a-z0-9-]`, starting/ending alphanumeric (see Validation). |
| `version` | u32 | Monotonic version (≥ 1). |
| `name` | string? | Editor label. |
| `trigger` | Trigger | How the flow is invoked (exactly one). |
| `entry` | node id | The node the trigger payload enters at. |
| `nodes` | Node[] | The graph steps. |
| `edges` | Edge[] | Wiring between output ports and downstream nodes. |
| `credentials` | CredentialRef[] | Declared by logical name; resolved by the vault (5.9). |

**Node** — `{ id, type, label?, config?, credential? }`. `type` is an **open
string** the runner's node library (5.3) resolves (`postgres-query`,
`transform`, `http-request`, `conditional`, `respond`, `delay`, `custom`, …).
`config` is an **opaque JSON object** typed by the node library, *not* by this
schema. `credential` optionally references a declared `CredentialRef` by name.

**Edge** — `{ from, from-port?, to, to-port? }`. `from-port` defaults to `main`;
`error` is the reserved **error path** (5.2); node types may define others (a
`conditional`'s `true`/`false`, an `evaluate`'s `out-of-spec`, …). **Branch** =
several edges from distinct ports of one node; **merge** = several edges into one
node. Cycles are allowed (loop/split/merge, 5.3).

**Trigger** — a tagged union (`"type"` discriminator):
- `webhook` `{ sync, path? }` — HTTP; `sync` responds within the request
  (write-ahead default, D15). *(F1)*
- `cron` `{ schedule }` — dispatcher-owned; wakes parked projects. *(F3)*
  **Misfire collapse:** dispatcher downtime spanning several ticks fires only the
  *latest* missed tick, never a catch-up burst — ticks are scheduling boundaries,
  not durable work items. Per-flow catch-up (replaying every missed tick) is a
  future ordering policy (rides wamn-1d4).
- `row-event` `{ table, event }` — durable outbox row event (D4, 5.14). *(F4)*
- `manual` — editor test-run.

**CredentialRef** — `{ name, kind?, description? }`. A logical name nodes point
at; the vault (5.9) resolves it to a lazy handle at run time. **No secret
material ever appears in flow data.**

### Example (canonical JSON)

```json
{
  "schema-version": "0.1",
  "flow-id": "s3-demo",
  "version": 1,
  "trigger": { "type": "webhook", "sync": true },
  "entry": "t",
  "nodes": [
    { "id": "t", "type": "transform", "config": { "op": "upper" } },
    { "id": "w", "type": "pg-write" },
    { "id": "c", "type": "conditional", "config": { "min-len": 3 } },
    { "id": "out", "type": "respond" }
  ],
  "edges": [
    { "from": "t", "to": "w" },
    { "from": "w", "to": "c" },
    { "from": "c", "from-port": "true", "to": "out" }
  ]
}
```

Worked examples for the POC flows live in
`crates/wamn-flow/tests/fixtures/`: F1 `receipt-received` (sync webhook, error
path, branch/merge), F3 `escalate-stale-holds` (cron + credential + egress),
F4 `disposition-recorded` (row-event + custom node + idempotent callback). Each
is round-tripped, validated, and checked against the published schema in
`crates/wamn-flow/tests/flows.rs`.

## Import / export

`Flow::from_json` / `Flow::to_json` are the canonical import/export. Export is
pretty-printed; default-valued fields (`from-port: "main"`, empty `config`,
empty `edges`/`credentials`, absent options) are omitted, so exported flows are
minimal and re-import to an identical value (round-trip).

## Validation

`Flow::validate` checks **graph structure** and returns typed
[`Issue`]s with a stable machine `code` and a JSON path. Severity: only
`error` makes a flow invalid; `warning` flags editor-fixable smells.

- **Errors:** unsupported `schema-version`, empty `flow-id`, `flow-id` not a
  lowercase slug (`[a-z0-9-]`, starting and ending alphanumeric — flow ids are
  embedded verbatim into the 5.14 deterministic trigger run ids
  `{flow}:cron:{tick}` / `{flow}:outbox:{seq}`, so excluding `:` keeps the
  flow-id prefix unambiguous to parse and ASCII-only keeps id ordering
  collation-independent; enforced here in `validate()`, not in the published
  JSON Schema — the `0.1` contract stays additive), `version < 1`,
  empty/duplicate node id, empty node `type`, no nodes, duplicate credential
  name, node referencing an undeclared credential, `entry` not a node, edge
  endpoint not a node, self-loop, empty `cron` schedule / `row-event` table.
- **Warnings:** node unreachable from `entry` (dead node).

It deliberately does **not** validate per-node-type `config` — that is the node
library's job (5.3), which will contribute config schemas keyed by `type`. This
keeps 5.1 decoupled from the node set.

## Diff

`diff(old, new)` produces a structured `FlowDiff` — nodes added / removed /
changed (with which of `type`/`config`/`credential` changed), edges added /
removed (identity = the full `from/from-port/to/to-port` tuple), and
entry / trigger / credential-set changes. This is the editor's version-diff view
(5.8) and the input to schema-impact analysis (11.8), not a text diff.

## Versioning & compatibility

Two independent version numbers: a flow's own `version` (monotonic per
`flow-id`), and the schema **format** `schema-version` (this document: `0.1`).
Compatibility mirrors the WIT freeze — `0.1.x` is additive/clarifying only
(new optional fields, new trigger/node conventions); any breaking change (new
required field, renamed/removed field, changed edge identity) waits for `0.2`.
The validator rejects a `schema-version` with a newer major or minor than it
implements.

## Relationship to the S3 stand-in and downstream

The S3 flow-runner PoC (`components/flowrunner`, wamn-lsf) used a minimal ad-hoc
JSON (`{version, nodes:[{id,type,config}], edges:[[from,to]]}`) as an explicit
stand-in. This schema is the canonical replacement: triggers become a typed
top-level field (not a `webhook-in` node), edges gain output ports (branch /
error paths), and credentials are first-class. The production runner (5.2)
adopts `wamn-flow`; the S3 flowrunner is left as-is until then.

## Regenerating the contract

```sh
cargo run -p wamn-flow --example print-schema > docs/flow-schema.schema.json
```

`flows.rs::committed_schema_matches_types` fails if the committed file drifts
from the Rust types.

## References

- Plan: `docs/platform-plan.md` §Epic 5 (5.1–5.3, 5.14), D1 (flow-as-IR).
- POC flows: `docs/poc-material-receiving.md` (F1–F4).
- Node contract: `docs/wamn-node.wit` (payload/config/credential handles).
- S3 stand-in: `components/flowrunner`, `docs/p0-results.md` S3.

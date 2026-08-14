# Flow Graph Schema — canonical `0.1`

The canonical flow document is a versioned directed graph of typed nodes wired
by ported edges. It declares portable connection requirements only; endpoint,
credential, trusted host policy, and network policy facts live outside the
artifact.

- **Contract file:** [`flow-schema.schema.json`](../contracts/flow-schema.schema.json)
  is generated from `crates/execution/flow-model`.
- **Crate:** `crates/execution/flow-model` owns types, import/export,
  validation, version diff, the digest preimage, and schema generation.

## Model

| Field | Type | Notes |
|---|---|---|
| `schema-version` | string | Flow-schema format version. The MVP contract remains `0.1`. |
| `flow-id` | string | Stable across versions; lowercase slug, starting and ending alphanumeric. |
| `version` | u32 | Monotonic version, `>= 1`. |
| `name` | string? | Editor label; not identity. |
| `nodes` | Node[] | Exactly one `request` or `event` entry node. |
| `edges` | Edge[] | Wiring between output ports and downstream nodes. |
| `connection-requirements` | FlowConnectionRequirement[] | Artifact-local portable requirements consumed by integrate nodes. |

**Node** — `{ id, type, label?, config?, connection? }`. `id` accepts exactly
lowercase ASCII letters, digits, and hyphens (`^[a-z0-9-]+$`). `type` is an
open string except for the engine-owned `request`, `event`, `respond`, `fail`,
and `call-flow` nodes. `config` is an opaque JSON object except for those
engine-owned nodes. `connection` names one artifact-local requirement and is
allowed only on integrate nodes.

**Edge** — `{ from, from-port?, to, to-port?, ordinal? }`. `from-port` defaults
to `main`; `error` is the reserved error path. `ordinal` is the explicit
fan-out order inside one `(from, from-port)` group and participates in artifact
identity.

**Call-flow config** — `{ flow-id }`. Callee content and binding facts do not
enter the caller flow document.

## Retired vocabulary

The greenfield MVP `0.1` contract refuses the retired vocabulary rather than
grandfathering it:

- top-level `trigger`, `entry`, `ordering`, `partition-policy`, `capture`,
  `credentials`, and `allowed-hosts`;
- node-level `credential`;
- cron entry nodes and `time-shift` examples;
- `InvokeFlowConfig` and every non-`{ flow-id }` call-flow shape.

Connection requirements remain the sole portable authority declaration in the
flow artifact. Their connection-type descriptor may still name
environment-owned credential injection, because those are binding requirements,
not flow-owned credential references.

## Validation

`Flow::validate` checks graph structure and returns stable machine-readable
issue codes. The published schema rejects unknown fields; validation additionally
checks entry cardinality, flow and node identifiers, connection requirement
shape and ordering, edge endpoints and ports, request input schemas, call-flow
config, terminal node config, and request-flow answerability.

## Diff and identity

`diff(old, new)` reports nodes, edges, and connection requirements added,
removed, or changed. `Flow::canonical_bytes` hashes a `FlowPreimage` projection:
node frames are ordered by node id, edges by their stable edge key, display text
is omitted, and portable connection requirements remain artifact identity.

## Regenerating the contract

```sh
cargo run -p wamn-flow --example print-flow-schema > docs/archive/contracts/flow-schema.schema.json
```

`flows.rs::committed_schema_matches_types` fails if the committed schema drifts
from the Rust types.

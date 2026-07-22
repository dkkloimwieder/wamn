# wamn-testkit — the flow/node test-case + assertion vocabulary (11.4)

`crates/wamn-testkit` is the **pure** vocabulary a flow or node test case is
written in. A case is *data* — a `TestCase` loads from a JSON file (the
`testkitbench` gate's `--cases` fixture) or a catalog `jsonb` column identically
— and `evaluate(case, captured) -> Outcome` is a pure fold of a captured fact
bundle into pass/fail. The gate is the effect shell that *fills* the
`Captured` bundle (a warm `ServeNode` invoke, a `RunWorker` drain, admin-pool DB
reads); the library only *decides*.

No DB / clock / wasm / host dependency. The status/kind taxonomy is REUSED
verbatim from `wamn-run-store` (`RunStatus` / `FailKind` / `NodeErrorKind`) and
the run-context mirrors `wamn-node-invoke`'s `WireRunContext`, so an assertion is
stated in the same enums the runner records and the node contract freezes.

## Case format

```jsonc
{
  "schema-version": "0.1",              // case-format version (defaults to 0.1)
  "name": "disposition-reject",         // human id, unique within a suite
  "node-ref": { "node-id": "recommend" }, // present ⇒ node-level case
  // "flow-ref": { "flow-id": "poc-s6", "version": 1 }, // OR flow-level
  "input": { "hold": { "moisture_pct": "12.00" } },     // node input / flow trigger
  "config": null,                       // optional node config document
  // "ctx": { … WireRunContext … },     // optional explicit run-context
  "expect": [ /* assertions */ ],
  // "normalize": { "canonicalize": true, "ignore-paths": ["/meta/run-id"] } // 11.3, optional
}
```

- `node_ref` present ⇒ **node-level** case: the gate drives the pure
  `run(ctx, input)` handler in a warm `ServeNode` and captures the emission,
  port, or error.
- `flow_ref` present ⇒ **flow-level** case: the gate drives the flow under the
  test-double set (virtual clock + seeded random + egress recorder) and captures
  the run outcome, egress log, and admin-pool DB reads.

`SCHEMA_VERSION` is `0.1` and mirrors the `wamn-catalog` precedent: `0.1.x` is
additive/clarifying only; a breaking wire change waits for `0.2`.

## Matcher semantics

Every assertion is an externally-tagged, kebab-case enum variant.

### Node output

| Assertion | Wire | Passes when |
| --- | --- | --- |
| `Equals(v)` | `{"equals": v}` | the node output equals `v` exactly (deep JSON equality) |
| `Subset(v)` | `{"subset": v}` | the node output deep-subset-matches `v` (see below) |
| `PathEquals{pointer,value}` | `{"path-equals": {"pointer": "/a/b", "value": v}}` | the output at RFC-6901 `pointer` equals `value` |
| `Port(p)` | `{"port": "main"}` | the emission port equals `p` (the absent/default port is captured as the literal `main`) |

**Subset semantics** (`subset_match`), pinned by a drift-guard test:
- objects — every key in `expected` is present in `actual` and recursively
  subset-matches; extra actual keys are ignored;
- arrays — every element in `expected` subset-matches SOME element of `actual`,
  **order-insensitive, no length constraint**; extra actual elements are ignored;
- scalars — exact JSON equality.

### DB state

`{"db-state": {"query": "SELECT to_jsonb(t) FROM sink t", "params": [], "expect": …}}`

The harness runs `query` (with text `params`) through the ADMIN (superuser)
session and captures one JSON value per row — the query must select a **single
json column** (e.g. `to_jsonb(t)`). The evaluator correlates the assertion to its
capture by `(query, params)`.

| `expect` | Wire | Passes when |
| --- | --- | --- |
| `RowCount(n)` | `{"row-count": 1}` | the query returned exactly `n` rows |
| `FirstRow{row,subset}` | `{"first-row": {"row": {…}, "subset": true}}` | the first row equals `row` (or subset-matches it when `subset`) |
| `Empty` | `"empty"` | the query returned no rows |

**RLS distinction:** DB-state reads go through the provisioner's **superuser**
session (RLS-bypassing), scoped to the runner's tenant + schema — NOT the
runner's own `wamn_app` (NOSUPERUSER, RLS-forced) pool. A DB-state assert
observes the row a superuser sees.

### Egress

`{"egress": {"flow": "<workload-id>", "calls": …}}` — filters the recorded
outbound requests to the flow whose workload id is `flow`.

| `calls` | Wire | Passes when |
| --- | --- | --- |
| `ExactlyThese([m,…])` | `{"exactly-these": [{"authority": "…"}]}` | the flow's recorded calls are EXACTLY these — every call matched, every matcher used, counts agree. **"Nothing else": an EXTRA call fails.** The security regression. |
| `Includes([m,…])` | `{"includes": […]}` | the flow made AT LEAST these calls (an extra is allowed) |
| `NoneDenied` | `"none-denied"` | no recorded call for the flow was denied |
| `Count(n)` | `{"count": 1}` | the flow made exactly `n` recorded outbound calls |

An `EgressMatcher` is `{method?, authority?, path?}`: a present field must equal
the record's; an absent field is a wildcard.

### Error path

| Assertion | Wire | Passes when |
| --- | --- | --- |
| `ErrorClass{node_error}` | `{"error-class": {"node-error": "invalid-input"}}` | the node returned this frozen-taxonomy error kind |
| `RunOutcome{status,fail_kind?,fail_node?}` | `{"run-outcome": {"status": "failed", "fail-kind": "terminal"}}` | the run reached `status`; present `fail_kind`/`fail_node` are extra constraints |

## The node-level case shape (7se / 828 consume)

For hand-authoring node cases, `NodeCase` is the compact shape the **7se** lane
expresses and the **828** lane stores:

```jsonc
// ok, matched by subset, on the main port
{"name": "reject",
 "input": {"hold": {"moisture_pct": "12.00"}},
 "expect": {"ok": {"value": {"recommended": "reject"}, "match": "subset", "port": "main"}}}

// error, by frozen taxonomy kind
{"name": "bad-input",
 "input": {"hold": {"moisture_pct": "x"}},
 "expect": {"error": "invalid-input"}}
```

`NodeCase::into_test_case()` lowers it to a `TestCase`:
- an `ok` with `match: exact` → `Equals`; `match: subset` → `Subset` (`match`
  defaults to `exact`);
- a `port` → an additional `Port` assertion;
- an `error` → `ErrorClass`.

So the sibling lanes' reconcile is a **re-import, not a rewrite**: they express
`NodeCase` and call `.into_test_case()`, or lower to the canonical `TestCase`
vocabulary directly.

## Record-and-replay: pin a run (11.3)

A recorded run is a fixture for free. `pin_run(run, node_runs, opts)` (module
`pin`) is the PURE transform from a stored run (a `wamn_run_store` `RunRecord` +
its `node_runs`) to a canonical `TestCase`; the `wamn-ctl pin-run` verb is the
effect shell that reads the rows and writes the case into the flow's
`test_suites`/`test_cases` (11.2 storage). Dependency direction: this reads STORE
records and writes a testkit case, so it lives in testkit (which already depends
on `wamn-run-store`) — not in run-store, which would be a cycle.

The pinned case (minimal-correct v0 shape):
- **flow-level** — `flow-ref` = the run's `(flow_id, flow_version)`; `input` = the
  run's trigger input; `expect` = a `RunOutcome` (the run's terminal
  status/fail-kind/fail-node) PLUS, when the run recorded a replayable terminal
  node, an `Equals` over that node's emission (the reconstruction-relevant
  payload, where volatile ids live); `normalize` = `canonicalize` on + any caller
  `ignore-paths`.
- 9.6 captures NODE I/O only — egress + DB state are filled by the LIVE harness,
  not `node_runs`, so those assertions cannot be pinned from history in v0.
  `Captured::node_output` is a single value (no whole-run node map), so a
  multi-node run pins the FLOW outcome + its TERMINAL node output, not a per-node
  map. Both are deliberate v0 scoping.

**Secret redaction at pin time.** Every payload that becomes part of the case (the
trigger input, the pinned node output) is passed through
`wamn_run_store::capture::scrub` first, so a pinned case NEVER contains a secret
even from a `full`-capture run (where the stored `node_runs` payloads are
faithful). Scrub is idempotent — a `scrubbed` row is safe to re-scrub.

**Capture-mode policy.** `off`/`preview` → the terminal node has no stored output
→ `PinError::NotCaptured` (nothing written); `scrubbed`/`full` → pin
(re-scrubbed).

### Normalization (volatile fields)

A case's optional `normalize` is applied SYMMETRICALLY to the expected value and
the captured node output by `evaluate` before a node-output assertion
(`Equals`/`Subset`/`PathEquals`) compares them — a no-op for run-outcome / egress
/ db-state. Two knobs, both pure (`serde_json` only, guest-compilable, **no
regex**):

| field | wire | effect |
| --- | --- | --- |
| `ignore-paths` | `["/meta/run-id", "/xs/1"]` | drop each RFC-6901 pointer from BOTH sides (an unresolved pointer is a no-op) |
| `canonicalize` | `true` | replace UUID-shaped (`8-4-4-4-12` hex) and narrow RFC-3339-`Z` timestamp string leaves with `[uuid]` / `[timestamp]` on BOTH sides |

Because normalization runs on both sides, a same-shaped volatile value (a fresh
UUID, a later timestamp) collapses to the placeholder and matches, while a REAL
field difference survives — this is the record-and-replay round-trip: pin a run,
rebuild a `Captured` from its recorded facts, and `evaluate` passes; a mutated
volatile field still passes, a mutated real field fails.

## Running the gate

See `docs/build-and-test.md` → **[11.4] assertion library (testkitbench)** and
**[11.3] record-and-replay fixtures (pinproof)**.

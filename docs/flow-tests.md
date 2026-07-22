# Flow test suites (11.2) — test cases as catalog data

A flow's test cases live as **catalog data**, stored in Postgres and versioned
WITH the flow they test. A flow version and its test suite promote together
between environments through the same `copy-project-env --include definition`
path that carries catalogs, flows, RLS policies, and event registrations.

## Storage (`deploy/sql/flow-tests.sql`)

Two tables in the `wamn_run` schema (rewritten to the project schema on
provisioning, the `publish-catalog --runstate` convention), additive to
`deploy/sql/flows.sql`:

- **`wamn_run.test_suites`** — `(tenant_id, flow_id, flow_version, suite_id,
  name, …)`, PK `(tenant_id, flow_id, flow_version, suite_id)`, FK
  `(tenant_id, flow_id, flow_version) → wamn_run.flows(tenant_id, flow_id,
  version) ON DELETE CASCADE`.
- **`wamn_run.test_cases`** — the suite key columns + `(case_id, ordinal,
  case_body)`, PK `(tenant_id, flow_id, flow_version, suite_id, case_id)`, FK
  `(tenant_id, flow_id, flow_version, suite_id) → wamn_run.test_suites(…) ON
  DELETE CASCADE`.

Both carry the platform security floor: `ENABLE` + `FORCE ROW LEVEL SECURITY`, a
tenant policy keyed on `NULLIF(current_setting('app.tenant', true), '')`,
`CHECK (tenant_id <> '')`, and `GRANT SELECT/INSERT/UPDATE/DELETE TO wamn_app` —
the exact shape of `flows.sql`.

`flow_version` is **denormalized** onto `test_cases` (it is part of the composite
FK to the suite, not reached only through a join to `test_suites`) — the
`event_registrations` precedent: the promote-copy can scope cases by version
without a join, and the FK makes the binding structural.

## Version binding

Every suite and case row pins a concrete `(flow_id, flow_version)`. There is **no
"active suite" pointer** in v0: a suite tests one specific flow version, full
stop. The `test_suites/test_cases → flows` FK `ON DELETE CASCADE` makes the
binding structural — dropping a flow version takes its suites and their cases
with it (proven live: `wamn-ctl tests/suite_promote_live.rs`, `wamn-gates
suiteproof`).

## Promote semantics (`copy-project-env --include definition`)

The definition copy (`crates/wamn-ctl/src/copy_project_env.rs`,
`exec_copy_definition`) enumerates its artifacts explicitly. Order is
FK-significant:

1. applied catalogs (2.5 migrate engine)
2. **flows** (verbatim row copy) ← the FK target for suites
3. RLS policy rows (+ re-compile/apply)
4. event registrations (verbatim)
5. **test suites, then test cases** (verbatim, `INSERT … ON CONFLICT DO UPDATE`)

Because flows are installed in block 2, a suite copied in block 5 always finds
its `(flow_id, flow_version)` present on the destination. Before any of this, a
**suite-orphan guard** (block 0, the D24 shape) refuses the copy — naming the
orphaned suites, mutating nothing — if a carried suite pins a flow version the
destination will hold in NEITHER the src flow registry (what block 2 installs)
NOR the dst's existing flows. The pure decision is
`wamn_migrate::check_suite_orphans`; the driver read builders are
`wamn_migrate::sql::select_suites_for_tenant_sql` /
`select_flow_versions_for_tenant_sql`. `verify` compares suite/case row counts
between src and dst.

The FK is the structural backstop; the guard converts what would be a bare
mid-copy FK error into a clean, named refusal before any mutation.

## The envelope (`crates/wamn-flow-tests`)

`TestSuite` / `CaseEntry` are the pure import/export shape over the rows:

```json
{
  "schema-version": "0.1",
  "flow-id": "escalate-holds",
  "flow-version": 1,
  "suite-id": "smoke",
  "name": "escalate-holds smoke suite",
  "cases": [
    { "case-id": "escalates-stale", "ordinal": 0, "case": { "input": {…}, "expect": {…} } }
  ]
}
```

`TestSuite::from_json` validates the schema-version discriminator, non-empty ids,
unique case ids, and coherent (unique) ordinals; `to_json` round-trips.
`SCHEMA_VERSION` is `0.1` (mirrors `wamn_catalog::SCHEMA_VERSION` / the flow-schema
freeze: `0.1.x` is additive-only).

## The gyt vocabulary seam (v0 opaque case body)

In v0 the case **body** (`CaseEntry::case`, stored in `test_cases.case_body`
jsonb) is an opaque `serde_json::Value`: this crate validates the ENVELOPE, not
the body. The canonical case/assertion vocabulary is a sibling crate,
`wamn-testkit` (lane gyt). At integration, `wamn-flow-tests` gains a
validate-on-write pass that parses each `case` against those serde types — the
one seam this crate is designed to grow into. Until then any well-formed JSON is
accepted as a body.

## What v0 does NOT include

- an "active suite" pointer (suites pin a version);
- suite **execution** from catalog data — running a stored suite against a live
  flow composes gyt's testkit runner (`testkitbench`) later, the natural
  follow-up (likely already covered by the 3rj capstone).

## Gates

- **`wamn-ctl tests/suite_promote_live.rs`** — drives the REAL
  `copy-project-env --include definition` across two project-env databases:
  promote (flow v1 + suite/cases arrive version-bound, counts match), RLS (a
  second tenant sees zero suites), FK cascade, and the guard refusal. Recipe:
  `docs/build-and-test.md` [11.2 / wamn-828].
- **`wamn-gates suiteproof`** — the in-cluster gate-of-record candidate: the same
  arc in an ephemeral schema against `WAMN_PG_URL` / `WAMN_PG_ADMIN_URL`
  (`deploy/gates/suiteproof-job.yaml`).

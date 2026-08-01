# Scenario catalog — persisted suites and cases

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
with it (proven live:
`services/ctl/tests/suite_promote_live.rs`, `wamn-gates suiteproof`).

## Promote semantics (`copy-project-env --include definition`)

The definition copy (`services/ctl/src/copy_project_env.rs`,
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
`wamn_schema_control::check_suite_orphans`; the driver read builders are
`wamn_schema_control::sql::select_suites_for_tenant_sql` /
`select_flow_versions_for_tenant_sql`. `verify` compares suite/case row counts
between src and dst.

The FK is the structural backstop; the guard converts what would be a bare
mid-copy FK error into a clean, named refusal before any mutation.

## The envelope (`crates/scenarios/model`)

`TestSuite` / `CaseEntry` are the pure import/export shape over the rows:

```json
{
  "schema-version": "0.1",
  "flow-id": "escalate-holds",
  "flow-version": 1,
  "suite-id": "smoke",
  "name": "escalate-holds smoke suite",
  "cases": [
    {
      "case-id": "escalates-stale",
      "ordinal": 0,
      "case": {
        "name": "escalates-stale",
        "flow-ref": { "flow-id": "escalate-holds", "version": 1 },
        "input": { "hold-id": "h-1" },
        "expect": [
          { "run-outcome": { "status": "completed" } }
        ]
      }
    }
  ]
}
```

`TestSuite::from_json` validates the schema-version discriminator, non-empty ids,
unique case ids, and coherent (unique) ordinals; `to_json` round-trips.
`SUITE_SCHEMA_VERSION` is `0.1` (mirrors `wamn_schema_model::SCHEMA_VERSION` / the flow-schema
freeze: `0.1.x` is additive-only).

## Model and persistence ownership

`wamn-scenario-model` owns `TestSuite`, `CaseEntry`, and `TestCase`; it validates
the envelope and each case's version, identifiers, exclusive target, and
assertion shape before write. `wamn-scenario-catalog` owns the SQL read/upsert
contracts, ordering, durable/run compatibility translations, and pin-from-run
transform. The database still stores each validated case body verbatim as
`jsonb`.

## What v0 does NOT include

- an "active suite" pointer (suites pin a version).

Stored suite execution is provided by the separate `wamn-scenario-worker`
artifact using `wamn-scenario-runtime` adapters and the production flowrunner
component. Its `--execution-schema-template` maps each case ordinal to a
distinct caller-provisioned schema, preserving case isolation without granting
the worker schema-creation privileges.

### DbState POC resource envelope

`wamn-scenario-runtime` applies one fixed exploratory-development policy to
every stored DbState assertion: a 5 second PostgreSQL `statement_timeout`, at
most 256 rows, and at most 1 MiB of serialized JSON text. These are safety rails,
not production capacity or latency promises.

The policy is deliberately conservative relative to the checked-in POC evidence
at baseline `48a3478`. Across `poc-f{1,3,4}-suite.json` and
`testkit-cases.json`, ten DbState assertions are stored; every assertion declares
at most one result row, and the largest declared first-row JSON value is 34
bytes. Reproduce that inventory with:

```bash
jq -s '[.. | objects | select(has("db-state")) | .["db-state"]] |
  {assertions:length,
   max_declared_row_count:
     (map(.expect["row-count"] //
       (if .expect["first-row"] then 1 else 0 end)) | max),
   max_expected_first_row_json_bytes:
     (map(.expect["first-row"].row? | select(. != null) |
       tojson | utf8bytelength) | max)}' \
  deploy/gates/poc-f1-suite.json deploy/gates/poc-f3-suite.json \
  deploy/gates/poc-f4-suite.json deploy/gates/testkit-cases.json
```

Each assertion runs in its existing tenant-scoped read-only transaction. The
runtime sets the timeout transaction-locally, asks PostgreSQL for at most one
sentinel row beyond the limit, and streams rows instead of collecting them.
PostgreSQL replaces a single over-limit JSON value with a small marker before
protocol transfer; checked cumulative accounting rejects the row before parsing
or retaining it. Any timeout, row, byte, cancellation, dependency, or result
shape failure rolls the transaction back and discards the assertion's partial
capture.

## Gates

- **`services/ctl/tests/suite_promote_live.rs`** — drives the REAL
  `copy-project-env --include definition` across two project-env databases:
  promote (flow v1 + suite/cases arrive version-bound, counts match), RLS (a
  second tenant sees zero suites), FK cascade, and the guard refusal. Recipe:
  `docs/build-and-test.md` [11.2 / wamn-828].
- **`wamn-gates suiteproof`** — the in-cluster gate-of-record candidate: the same
  arc in an ephemeral schema against `WAMN_PG_URL` / `WAMN_PG_ADMIN_URL`
  (`deploy/gates/suiteproof-job.yaml`).

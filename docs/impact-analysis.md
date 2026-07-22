# Schema-change impact analysis (11.8)

Before a migration applies, answer *what breaks if I apply this*: which flows,
suites, and generated-API resources depend on the entities the migration changes.
The analysis is a **JOIN** over data the platform already stores — it holds no
connection, clock, or wasm.

- **Issue:** wamn-wvb `[11.8]`; **Epic:** E11 Integrated Testing.
- **Pure crate:** `crates/wamn-impact` — the decision (`analyze` → `ImpactReport`).
- **Effect shell:** `crates/wamn-ctl` — `impact-report` (read-only verb) and the
  `migrate-catalog` render + `--acknowledge-impact` gate read the rows and call it.
- **Consumers of the report's suite tuples:** wamn-0lfu (parked, "execution from
  stored suites") — this bead *enumerates* the suites that would run; it does not
  run them.

## The report

`wamn_impact::analyze(&ImpactInput) -> ImpactReport` groups the migration by
affected entity. Each `EntityImpact` carries the change (added / removed /
changed), its additive-vs-destructive classification, and the downstream
dependents:

```
schema-change impact — 2 affected entities
  [DESTRUCTIVE] entity "orders" (id "sales_orders") — changed
      api: /api/rest/orders
      api: /api/rest/lines?expand=order
      flow via registration: tenant "t1" flow "notify" (registration "reg-1")
      flow via node config:  tenant "t1" flow "sync" v3 node "read" (config entity "orders")
      suite: tenant "t1" flow "notify" v2 suite "smoke"
  [additive   ] entity "audit" (id "audit") — changed
      api: /api/rest/audit
      (no dependent flows or suites)
```

### The five edges

1. **affected entity + classification** — group the compiled plan's operations by
   `wamn_ddl::Operation::entity`; an entity is destructive iff any of its ops is
   `Safety::Destructive`. The plan is the authoritative source (its per-op
   `entity`/`field` attribution was pre-seeded for exactly this bead) — **no SQL
   re-parse**.
2. **flows via event registration** — id-keyed and **rename-proof**:
   registrations whose stable `entity_id` equals the affected entity's id
   (`catalog.event_registrations`, the `event_registrations_by_entity` index).
3. **flows via node config** — NAME-keyed and **NOT rename-proof**: an active
   flow's `postgres` node names its entity in `config["entity"]`, which the
   generated router resolves *by entity name* (`wamn_api` `entity_by_name`,
   `/api/rest/{name}`). A rename changes `entity.name` and silently dangles the
   ref — so the analysis matches an affected entity's **old and new** names and
   surfaces a dangling ref by the OLD name (a genuine report line, not an error it
   can fix). Only `postgres` carries a structured `entity` key today;
   `postgres-query` (raw author SQL) is scanned for forward-safety but never
   matches without one.
4. **suites of affected flows** — every `test_suites` row of a flow either edge
   touches, **all versions** (see the version rule below). The suite tuple keeps
   its `(tenant, flow_id, flow_version, suite_id)` so the parked executor can pin.
5. **generated-API resources** — pure over the catalog: the entity's own
   `/api/rest/{name}` plus, for each relation touching it, the neighbour's
   `?expand=` resource that embeds it (the router serves an embed on the *other*
   endpoint's resource). No `wamn-api` dependency — derived from the catalog alone.

### Which flow versions

The affected FLOWS are identified by `flow_id`:

- the **registration** edge is version-agnostic (a registration attaches to the
  live catalog, `flow_id`-keyed);
- the **node-config** edge scans **active** flow versions (the graph the runtime
  serves).

The **suite** edge then enumerates **every version** of those affected flows' test
suites — a flow version that is inactive but suite-bound is a legitimate dependent
(dropping/retyping the entity its flow reads still invalidates that suite). The
suite tuple preserves the version dimension for the executor.

## Tenant scoping

Entities are a **cross-tenant** axis: a shared entity table's change hits every
tenant that registered or built a flow on it. The shell reads registrations,
active flows, and suites **cross-tenant on the superuser connection** (RLS
bypassed, the D24 precedent), and each per-edge report line carries its tenant.
`migrate-catalog` has a single `--tenant` (the version it applies is
tenant-scoped), but the impact picture it renders is inherently multi-tenant.

## Wiring (wamn-ctl)

- **`impact-report`** — the read-only schema-designer surface. Reads the current
  applied catalog + a `--target`, compiles the plan, reads the edges, prints the
  diff + the impact report. **Mutates nothing.**
- **`migrate-catalog`** — **always renders** the impact report on both `--dry-run`
  and apply (the D24 guard slot is the precedent). `--acknowledge-impact` gates
  the APPLY: a **destructive** plan whose affected entities carry a dependent flow
  or suite is REFUSED (typed `ImpactNotAcknowledged`, non-zero exit) unless the
  flag is passed — read-only, before the apply transaction, so a refusal mutates
  nothing. It is orthogonal to `--confirm-with-backup` (that gate is about data
  loss; this is about downstream flows/suites). An additive plan, or a destructive
  plan with no dependents, never trips it. `--dry-run` *surfaces* the gate (it is
  overridable, like `--confirm-with-backup`) rather than failing on it — unlike the
  unconditional D24 orphan refusal a dry run also runs and fails on.

## The suite-execution seam (parked)

Suite EXECUTION is out of scope (wamn-0lfu, "execution from stored suites"). The
report enumerates the exact `(tenant, flow_id, flow_version, suite_id)` tuples that
WOULD run; `ImpactReport.suites` is that executor's input contract. No consumer
executes stored `test_cases` today.

## Verification

- Pure unit tests (`crates/wamn-impact`) carry the bulk: the touched/untouched
  partition, destructive classification, the name-keyed node-config edge (id≠name
  fixture), the rename-surfaces-old-name case, the API-resource enumeration, and
  the acknowledge decision — each of the three mutants killed by a named test.
- The `$n` read builders are pinned against the schema of record by drift-guard
  tests in `crates/wamn-migrate/src/sql.rs` (the `include_str!` mirror of the gates
  `schema_drift` discipline).
- Live gate: `wamn-ctl tests/impact_report_live.rs` (throwaway PG). In-cluster
  gate: `wamn-gates impactproof` + `deploy/gates/impactproof-job.yaml`. Commands:
  `docs/build-and-test.md` [11.8].

## References

- Plan: `docs/platform-plan.md` §Epic 11 (11.8).
- The plan model (the input): `docs/ddl-compiler.md`, `crates/wamn-ddl`.
- The registration edge: `crates/wamn-event-reg`, `docs/build-and-test.md` [EVT-REG/D24].
- The suite storage: `docs/flow-tests.md`, `deploy/sql/flow-tests.sql`.

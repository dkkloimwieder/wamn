# wamn docs

The design source of truth. Start with the four documents below; the subsystem
docs-of-record follow. One doc per subsystem, each with a predictable name.

## Start here

- **[platform-plan.md](platform-plan.md)** — the architecture and roadmap: the
  planes, the epics, and the spec baseline. *What is this platform, and what is
  the overall plan?*
- **[Decision Boundaries & Alternatives](platform-plan.md#decision-boundaries--alternatives-denoted)**
  — the decision table inside the plan. *Which architectural choices are decided
  or locked, and which alternatives were rejected — and why?* (Note: D4 is
  superseded by D19 — CDC via logical decoding replaces the outbox trigger path.)
- **[core-pivot-plan.md](core-pivot-plan.md)** — the live work-ordering ledger,
  currently suspended by event-plane v3 Phase 0 (`wamn-l5i9`). *What are we
  working on right now, and in what order?*
- **[findings.md](findings.md)** — the single findings ledger and
  [status board](findings.md#0--status-board). *What is open, how bad, and what
  is next?* Its §6 is the current sequencing overlay (waves/clusters).

## Current docs by subsystem

| Subsystem group | Files |
|---|---|
| Catalog / schema | [catalog-model.md](catalog-model.md), [app-schema.md](app-schema.md), [schema-lifecycle.md](schema-lifecycle.md), [ddl-compiler.md](ddl-compiler.md), [migration-engine.md](migration-engine.md), [rls-builder.md](rls-builder.md), [seed-data.md](seed-data.md) |
| Execution | [flow-schema.md](flow-schema.md), [flow-runner.md](flow-runner.md), [node-library.md](node-library.md), [exec-ladder.md](exec-ladder.md), [run-queue.md](run-queue.md), [run-state.md](run-state.md), [wamn-node-design-notes.md](wamn-node-design-notes.md), [wamn-node.wit](wamn-node.wit) |
| Data path | [security-db-path.md](security-db-path.md), [wamn-postgres.wit](wamn-postgres.wit), [credential-vault.md](credential-vault.md) |
| Event plane | [event-plane-jetstream.md](event-plane-jetstream.md) (v3, current), [pg-walstream-fork.md](pg-walstream-fork.md) |
| Platform / infra | [platform-plan.md](platform-plan.md), [deployment-model.md](deployment-model.md), [postgres-topology.md](postgres-topology.md), [system-cluster.md](system-cluster.md), [registry-model.md](registry-model.md), [provisioning.md](provisioning.md), [wasmcloud-utilization.md](wasmcloud-utilization.md), [wash-runtime-fork.md](wash-runtime-fork.md), [api-gateway.md](api-gateway.md), [tracing.md](tracing.md) |
| POC | [poc-material-receiving.md](poc-material-receiving.md), [poc-f1.md](poc-f1.md), [poc-dm1.md](poc-dm1.md) |
| Process | [core-pivot-plan.md](core-pivot-plan.md), [findings.md](findings.md), [build-and-test.md](build-and-test.md), [p0-results.md](p0-results.md), [ceilings.md](ceilings.md) |

## Results & measurements

- **[p0-results.md](p0-results.md)** — P0 measurement record.
- **[ceilings.md](ceilings.md)** — capacity ceilings (raw CSVs in `ceilings-data/`).

**Provenance caveat:** these figures were measured with `fsync=off` — they are
**shape-only, not externally citable** (findings §1.3 / E6; durable re-measure
tracked as wamn-dzhw).

## Archive

Superseded material lives in [`archive/`](archive/), each file keeping a
one-line header (superseded by what, when, retained for what).

- **[archive/event-plane-v2-outbox.md](archive/event-plane-v2-outbox.md)** — the
  v2 event-plane doc, superseded by v3 ([event-plane-jetstream.md](event-plane-jetstream.md)).
  Retained for the outbox-era rationale and the teardown list's provenance.
- **[archive/p0-exit-criteria.md](archive/p0-exit-criteria.md)** — P0 is closed
  and its results live in [p0-results.md](p0-results.md). Retained for the
  go/no-go thresholds that gate re-measurement.
- **[archive/review-findings.md](archive/review-findings.md)** — the R-series,
  absorbed by [findings.md](findings.md). Retained for commit-message resolution.
- **[archive/structure-review.md](archive/structure-review.md)** — the SR-series,
  absorbed by [findings.md](findings.md). Retained for commit-message resolution.

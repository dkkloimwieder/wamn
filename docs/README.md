# wamn docs

The design source of truth. Start with the four documents below; the subsystem
docs-of-record follow. One doc per subsystem, each with a predictable name.

## Start here

- **[PLAN/PLAN.md](PLAN/PLAN.md)** — the authoritative post-POC roadmap and
  decision map (items 0–11). *What are we working on, in what order, and what
  is decided, directional, or open?* (The pivot-era ordering ledger is archived
  at [archive/core-pivot-plan.md](archive/core-pivot-plan.md).)
- **[FLOW-SPEC.md](execution/FLOW-SPEC.md)** (rev 18, normative) and
  **[POC-PLAN.md](poc/POC-PLAN.md)** (r6) — the callable-flow protocol and its
  proven POC ladder: the entry gate every PLAN item builds on.
- **[findings.md](findings.md)** — the single findings ledger and
  [status board](findings.md#0--status-board). *What is open, how bad, and what
  is next?*
- **[platform-plan.md](platform-plan.md)** — the decision archive of record:
  the **D1–D24 decision table** (rejected alternatives included) and the E1–E11
  epic anchor. Roadmap authority transferred to
  [PLAN/PLAN.md](PLAN/PLAN.md) on 2026-07-31; the D-number rows remain the
  full decisions the plan's one-line restatements point back to.

## Roadmap & upgrade plans (`PLAN/`)

- **[PLAN/PLAN.md](PLAN/PLAN.md)** — see above.
- **[PLAN/WASMCLOUD-UPGRADE-2.6.0.md](PLAN/WASMCLOUD-UPGRADE-2.6.0.md)** /
  **[PLAN/WASMCLOUD-UPGRADE-2.6.1.md](PLAN/WASMCLOUD-UPGRADE-2.6.1.md)** — the
  fork upgrade base plan and its 2.6.1 delta (epic `wamn-g2br`, closed;
  retirement decision tracked in bd).

## Current docs by subsystem

| Subsystem group | Files |
|---|---|
| Catalog / schema | [catalog-model.md](schema/catalog-model.md), [app-schema.md](schema/app-schema.md), [schema-lifecycle.md](schema/schema-lifecycle.md), [ddl-compiler.md](schema/ddl-compiler.md), [migration-engine.md](schema/migration-engine.md), [rls-builder.md](schema/rls-builder.md), [seed-data.md](schema/seed-data.md), [catalog-model.schema.json](contracts/catalog-model.schema.json) |
| Execution | [flow-schema.md](execution/flow-schema.md), [flow-runner.md](execution/flow-runner.md), [node-library.md](execution/node-library.md), [run-queue.md](execution/run-queue.md), [run-state.md](execution/run-state.md), [wamn-node-design-notes.md](execution/wamn-node-design-notes.md), [wamn-node.wit](contracts/wamn-node.wit), [flow-schema.schema.json](contracts/flow-schema.schema.json), [wamn-node-manifest.schema.json](contracts/wamn-node-manifest.schema.json) |
| Node supply chain | [builder.md](platform/builder.md) |
| Data path | [security-db-path.md](data-path/security-db-path.md), [wamn-postgres.wit](contracts/wamn-postgres.wit), [credential-vault.md](data-path/credential-vault.md) |
| Event plane | [event-plane-jetstream.md](events/event-plane-jetstream.md) (v3, current), [pg-walstream-fork.md](events/pg-walstream-fork.md), [wamn-jetstream.wit](contracts/wamn-jetstream.wit) |
| Observability | [tracing.md](observability/tracing.md), [metrics.md](observability/metrics.md), [dashboards.md](observability/dashboards.md) |
| Testing / scenarios | [scenario-model.md](testing/scenario-model.md), [scenario-catalog.md](testing/scenario-catalog.md), [impact-analysis.md](testing/impact-analysis.md) |
| Platform / infra | [platform-plan.md](platform-plan.md), [deployment-model.md](platform/deployment-model.md), [postgres-topology.md](platform/postgres-topology.md), [system-cluster.md](platform/system-cluster.md), [registry-model.md](platform/registry-model.md), [provisioning.md](platform/provisioning.md), [wasmcloud-utilization.md](platform/wasmcloud-utilization.md), [wash-runtime-fork.md](platform/wash-runtime-fork.md), [api-gateway.md](platform/api-gateway.md) |
| POC | [POC-PLAN.md](poc/POC-PLAN.md), [poc-material-receiving.md](poc/poc-material-receiving.md), [poc-f1.md](poc/poc-f1.md), [poc-dm1.md](poc/poc-dm1.md) |
| Process | [PLAN/PLAN.md](PLAN/PLAN.md), [findings.md](findings.md), [build-and-test.md](build-and-test.md), [p0-results.md](results/p0-results.md), [ceilings.md](results/ceilings.md) |

## Results & measurements

- **[p0-results.md](results/p0-results.md)** — P0 measurement record.
- **[ceilings.md](results/ceilings.md)** — capacity ceilings (raw CSVs in `ceilings-data/`).

**Provenance caveat:** these figures were measured with `fsync=off` — they are
**shape-only, not externally citable** (findings §1.3 / E6; durable re-measure
tracked as wamn-dzhw).

## Archive

Superseded material lives in [`archive/`](archive/), each file keeping a
one-line header (superseded by what, when, retained for what). The two frozen
external review inputs are the exception: they carry no header because
findings.md pins them by SHA-256.

- **[archive/core-pivot-plan.md](archive/core-pivot-plan.md)** — the 2026-07
  pivot-era work-ordering ledger, superseded by [PLAN/PLAN.md](PLAN/PLAN.md).
  Retained for wave records and sequencing provenance.
- **[archive/event-plane-v2-outbox.md](archive/event-plane-v2-outbox.md)** — the
  v2 event-plane doc, superseded by v3 ([event-plane-jetstream.md](events/event-plane-jetstream.md)).
  Retained for the outbox-era rationale and the teardown list's provenance.
- **[archive/exec-ladder.md](archive/exec-ladder.md)** — the predecessor
  execution-ladder proof, superseded by callable-flow rev18 and the F0–F4
  acceptance campaign. Retained as historical proof context.
- **[archive/p0-exit-criteria.md](archive/p0-exit-criteria.md)** — P0 is closed
  and its results live in [p0-results.md](results/p0-results.md). Retained for the
  go/no-go thresholds that gate re-measurement.
- **[archive/review-findings.md](archive/review-findings.md)** — the R-series,
  absorbed by [findings.md](findings.md). Retained for commit-message resolution.
- **[archive/structure-review.md](archive/structure-review.md)** — the SR-series,
  absorbed by [findings.md](findings.md). Retained for commit-message resolution.
- **[archive/REVIEW-260723.md](archive/REVIEW-260723.md)** — frozen external
  review input, SHA-256-pinned and assessed in findings §A.1/§B.6/§I. Moved
  byte-identical; evidence, not a decision.
- **[archive/RESTRUCTURE-260723.md](archive/RESTRUCTURE-260723.md)** — frozen
  external restructure proposal, SHA-256-pinned and assessed in findings
  §I/§U/§V. Moved byte-identical; evidence, not a decision.
- **[archive/REVISED-REVIEW.md](archive/REVISED-REVIEW.md)** — orphaned
  amendment of an earlier external review. Retained for review-era provenance.

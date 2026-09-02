# POC application portfolio — capability coverage plan

Status: DRAFT 2026-09-01 · follows the Receiving POC (wamn-10yt) ·
each application is one platform base package (+ optional overlay) and
exists to prove named capabilities. Build in coverage order; stop when
the capability list in `poc-architecture-review.md` §5 is proven.

## Coverage matrix

| # | Application | Capabilities exercised | New interfaces/seams | Drivers / simulators |
|---|---|---|---|---|
| 1 | **WMS** — move/adjust/merge/split, label scan/render, quantity aggregates by status/product/location | Multi-row transactional commands under contention; optimistic concurrency; sort/filter vocabulary at real breadth; **pure transform palette node** (ZPL/label render) composed with package ops in a wiring — the low-code proof | Blobstore seam (`wasmcloud:blobstore@0.1.0`) for rendered labels; array-of-scalar types (R-B) | Scan-event generator (handheld simulator: sequences of scan → move with configurable duplicate/out-of-order rates); seed inventory fixture |
| 2 | **Production counts** — machine data as production values against dispatched work orders | Streaming ingress: JetStream pull consumer, ack-after-process, DLQ; at-least-once + idempotency under measured duplicate rates; windowed rollups (hour/shift); backpressure | **MQTT ingress seam** (new protocol — seam-template test); time-series projection SQL (window functions) in the corpus | MQTT machine simulator (N lines × cycle counts, faults, restarts, replayed duplicates); work-order dispatch fixture |
| 3 | **Traceability / genealogy** — lot → sub-lot → consumed-in → shipped-to; recall query | Graph traversal (recursive CTE) in the verified corpus; immutable audit proof; **multi-package composition** (receiving + production + WMS in one release) — the composition test; cross-package dependency alias | `base_dependencies` > 1 (lifts the POC single-base restriction, by ruling) | Genealogy generator producing lot trees from apps 1–2 events; recall scenario script |
| 4 | **Quality / SPC** — samples, control limits, OOC alerts, holds | Event-handler fan-out chains (sample → OOC → hold → notify); derived-event consumers; windowed statistics; **per-user row scope** (inspector sees own plant → R-G) | Notification seam (email/webhook via `wamn:connection/http`); first RLS-bound user claims | Sample-result generator with injected drift/OOC patterns; plant/inspector identity fixture |
| 5 | **Maintenance / CMMS** — assets, PM schedules, work requests, downtime | Hierarchical data; **scheduled automations** (cron-shaped trigger kind); attachments | Time-driven automation kind; blobstore (photos) | Asset tree fixture; clock-driven scheduler harness; downtime event feed from app 2 |
| 6 | **Andon / shift handover** — live status, escalation, acknowledgement | Live-view tap under load; real-time UI push (SSE/WebSocket); presence | Server-push route kind | Status-change generator; browser-side load client |
| 7 | **ERP integration** — orders in, receipts/consumption out, reconciliation | `integration.*` component split (requirement-set rule); outbound HTTP retry/idempotency against an unreliable partner; reconciliation projection | Outbound HTTP seam hardening (retry policy by ruling); webhook ingress | Mock ERP (HTTP; configurable latency, 5xx, duplicate acks, schema drift) |
| 8 | **Time & labor** — clock in/out, job costing | Org-grain identity end-to-end; users across projects; per-user authorization | Identity epic (org directory, membership) as first consumer | Badge-scan generator; multi-project org fixture |

## Build order and exit gates

1. **WMS** — exit: a non-technical wiring composing `inventory.move` with a
   label-render palette node, gated, published, routed; contention test
   (two concurrent moves on one pallet) yields exactly one
   `concurrency_conflict`.
2. **Production counts** — exit: MQTT simulator at 500 msg/s with 5%
   duplicates yields byte-exact rollups; DLQ receives only poison.
3. **Traceability** — exit: three-package release; recall query returns
   the generated tree exactly; a breaking base change refuses by name.
4. **Quality** — exit: inspector A cannot read plant B rows at the
   database layer; OOC chain fires once per event under redelivery.

Apps 5–8 build only when their named capability has a consumer.

## Shared infrastructure to build once

- **Simulator framework**: one crate producing deterministic, seeded
  event streams (scans, machine counts, samples, badges) with knobs for
  rate, duplicates, reordering, faults. Every app's driver is a profile.
- **Mock external system**: one HTTP server profile-driven (ERP, MES,
  label printer) with injectable latency/failure.
- **Seed fixtures**: products, locations, work orders, assets, users —
  shared across apps as base package data.

## Rules

- Each app is a package with its own migrations, IR, generated
  artifacts; no shared hand-written schema.
- New seams follow the seam template (WIT + host plugin + admission
  facts); each is a ledger row with what it buys.
- Simulators drive real routes/consumers — never seed the database.
- Exit gates are measured; an app without a measurable gate is not built.

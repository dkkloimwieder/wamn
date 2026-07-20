# POC Spec: Material Receiving with Quality Hold

The first consumer of the platform, and the executable definition of done for the P1 walking skeleton and P2 product phase. Doubles as reference solution #1 (plan 10.5). **Scope guardrail:** this is a consumer, not a second roadmap — every feature exists to validate a platform primitive (see traceability matrix); anything that doesn't map to one is out of scope.

## Scenario
A manufacturer receives raw material shipments. The ERP announces each receipt; wamn validates the receipt against material specifications, places out-of-spec deliveries on quality hold, routes holds to inspectors through a web app, records dispositions, notifies the ERP, and escalates stale holds nightly.

## Personas & platform roles
- **ERP** (simulated by a script): fires webhooks, receives disposition callbacks. Machine client with an API key.
- **Inspector:** works the hold queue in the SPA. Application role `inspector`; RLS-scoped to their site.
- **Quality manager:** sees all sites, approves overrides. Application role `quality-manager`.
- **App builder** (us): platform RBAC `builder` on the project; a second account with `viewer` proves platform RBAC (8.1).

## Data model (Epic 3 exercise)
Extends the **system schema** — deliberately including the hard path of adding a column to a system entity:
- `users` + `cert_level` (enum: L1, L2) — system-entity extension.
- New entities: `sites`, `suppliers`, `materials` (spec fields as **unit-bound exact decimals**: `moisture_max_pct numeric`, `weight_tolerance_kg numeric` — validates 3.3's decimal/unit types), `receipts`, `receipt_lines` (`quantity numeric` + unit), `quality_holds` (status: open / disposed / escalated), `dispositions` (accept / reject / use-as-is, FK to hold + inspector).
- Relations: receipts→lines (1:N), lines→materials, holds→lines, dispositions→holds. One composite uniqueness constraint (`receipt_no, supplier_id`) — validates 3.3 catalog expressiveness.
- **RLS:** inspectors read/write holds only for their `site_id`; managers unrestricted; ERP API key can insert receipts and read dispositions only. Field-level mask: inspectors cannot see supplier pricing fields (4.3).
- Built twice: first via catalog API directly (P1, no UI), rebuilt via the schema designer UI (P2) — same catalog state proves the designer emits what the API accepts.

**Done (P1, wamn-521):** the API-first build ships as `poc/dm1` (`wamn-dm1`) — the promoted catalog + RLS policy + seed migrated live via 2.5 `migrate-catalog`, with a throwaway-Postgres gate proving site-scoped RLS, the ERP receipts gate, the composite unique, and exact-decimal specs (`docs/poc-dm1.md`). Two caveats carried: the `is-system` `users` extension lands as a data-schema table (follow-up wamn-5x0.3), and the role/site RLS claims are inert until the plugin injects them (4.2); the pricing field mask is 4.3.

## Flows (Epic 5 exercise)
**F1 — `receipt-received` (sync webhook, write-ahead default, D15).** ERP POSTs a receipt. Flow: validate payload (`invalid-input` on malformed) → upsert receipt + lines (transaction via `wamn:postgres` node) → evaluate each line against material specs → create `quality_holds` for out-of-spec lines → **synchronous response** `{receipt_id, holds: [...]}`. Exercises: sync direct dispatch, write-ahead audit row, transactions, exact-decimal comparison, error taxonomy.

**F2 — `disposition-recommendation` (custom code node).** Pure-compute Rust (or TS) node implementing `wamn:node`: takes hold + material history, returns a recommended disposition + confidence. **Imports nothing** (world `node`) — the builder must emit empty `hostInterfaces`, proving grant derivation. Invoked inside F4.

**F3 — `escalate-stale-holds` (cron, nightly).** Query holds open > 48h → mark `escalated` → notify manager via outbound HTTP (webhook-style notification) using a **stored credential** and `allowedHosts: [notify.example]`. Exercises: dispatcher-owned cron, parked-project wake (project idles overnight — scale-to-zero proof), credential vault, egress allowlist.

**F4 — `disposition-recorded` (row-event trigger).** CDC row event on `dispositions` insert (D19 event plane: reader → JetStream → materializer, via the flow's event registration) → flow calls F2's recommendation node (audit comparison: did inspector match the recommendation?) → POST callback to ERP with idempotency key. Exercises: CDC capture → materializer → doorbell path end-to-end, custom-node invocation, `rate-limited` handling (ERP simulator returns 429 with Retry-After on demand — assert shared throttle, no stampede).

## Frontend (Epic 6 exercise)
BYO React SPA from the starter template, generated TS SDK only (no hand-rolled fetch): login via platform IdP, inspector hold queue (filtered/paginated via generated REST), hold detail with disposition form (mutation), manager dashboard (relation expansion: hold → line → material → supplier). Deployed through the frontend build pipeline (6.2) to `{project}.wamn.example`.

## Tests (Epic 11 exercise)
- F1 suite: fixture pinned from a real run (secret-free by construction); assertions on node outputs, final DB state (hold created), and the sync response body.
- F4 suite: **egress spy** — exactly one call to the ERP callback URL, nothing else; 429 fixture asserting throttle behavior.
- F3 under **virtual time**: 48h passes in test wall-clock seconds.
- Custom node (F2): user-level unit tests run as builder publish gate.
- **Publish gate:** project policy "prod requires green suite" (11.7) — demonstrated by a deliberately failing promotion.
- **Schema impact demo:** stage a rename of `quality_holds.status` → impact analysis (11.8) flags F1/F3/F4 suites and the SPA's generated types before DDL applies.

## Acceptance script (the demo)
1. ERP simulator fires 20 receipts (3 out-of-spec) → sync responses list holds; runs table shows write-ahead rows.
2. Kill the host pod mid-burst → janitor marks the orphan `infrastructure-failure`; auditor query shows zero silent losses.
3. Inspector logs in, sees only their site's holds (RLS), disposes one → ERP receives exactly one callback (egress spy corroborates in test env).
4. Project idles; nightly cron wakes it from zero and escalates a stale hold (dispatch latency recorded vs. cold SLO).
5. Full test suite green in editor; failed-gate promotion demo; schema-impact demo.
6. Grafana: one trace threading webhook → runner → custom node → Postgres spans; per-flow dashboards live.

## Non-goals
No MQTT/OPC-UA (post-v0), no UI builder, no multi-site federation, no real ERP connector (simulator only), no frozen flows, no label printing/hardware.

## Traceability matrix (POC element → platform issues validated)
| POC element | Validates |
|---|---|
| `users.cert_level` extension | 2.4, 3.1 (system-entity extensibility) |
| Exact-decimal specs + composite unique | 3.3 (types, constraints) |
| Catalog-API-first, designer-UI-second build | 3.1→3.3 equivalence |
| Generated REST + SDK-only SPA | 4.1–4.5, 6.1–6.5 |
| Field mask on pricing | 4.3 |
| F1 sync + write-ahead + pod-kill demo | D15, 5.14 SLOs, 8.6 `infrastructure-failure` |
| F1 transactions + taxonomy | 2.1–2.2, wamn:node errors |
| F2 zero-import custom node | 5.4–5.6 (contract, builder, grant derivation) |
| F3 cron + parked wake + credential + allowlist | 5.14 dispatcher, scale-to-zero, 5.9, egress governance |
| F4 CDC-event→materializer→flow + 429 throttle | D19, 5.14, rate-limited semantics |
| RLS site scoping + ERP API key | 2.2, 4.2–4.3, 8.2 |
| Platform viewer account | 8.1 platform RBAC |
| Fixtures / egress spy / virtual time / gates / impact demo | 11.1–11.8 |
| Trace + dashboards + janitor audit | 9.1–9.6, 9.10, 8.6 |

**Phasing:** P1 builds the F1 slice + catalog-API data model + raw REST (no UI); P2 completes flows, SPA, editors, tests; the acceptance script is the P2 exit demo and design-partner showpiece.

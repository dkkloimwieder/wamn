# POC-F1: the `receipt-received` sync flow, end-to-end

Implements F1 of [docs/poc-material-receiving.md](poc-material-receiving.md) —
the P1 exit criterion (wamn-067): an ERP POSTs a receipt, the platform
validates it, upserts receipt + lines in one transaction, evaluates every line
against its material's specs with exact-decimal arithmetic, opens
`quality_holds` for out-of-spec lines, and answers `{receipt_id, holds: [...]}`
**within the request** (D15 sync, write-ahead default) — with every run
traceable (5.7 `runs`/`node_runs`), the results queryable through the generated
REST gateway (4.1), and the whole world provisioned from the catalog (3.x).

## Architecture

Three new pieces, each in the house shape:

| Piece | What it is |
|---|---|
| `poc/f1` | PURE F1 node logic: payload validation (no-float rule), exact-decimal arithmetic + spec evaluation, the parameterized SQL text the DB nodes run, inter-node/response JSON shapes. Unit-tested with no cluster. |
| `components/poc-webhook-f1` | The sync-webhook ingress: exports `wasi:http/incoming-handler` (deployed like the 4.1b gateway, routed by Host header), imports **only** `wamn:postgres` (2.6-clean), embeds the wamn-runner (5.2) engine and the five F1 node implementations. |
| `deploy/flows.sql` | The flow registry's production home (`flows`: tenant, flow_id, version, active, graph_json — the shape the dispatcher's `active_flows_sql` and the flowrunner already read; the a52 smoke's stand-in DDL is retired). Standalone + additive to `run-state.sql`. |

`publish-catalog` grew into the one project-provisioning tool: `--provision`
(3.2 floor) + `--runstate` (the **canonical** `deploy/run-state.sql` +
`deploy/flows.sql`, `include_str!`'d and dot-anchored-rewritten from `wamn_run`
to the project schema; `.dockerignore` now ships `deploy/` into the image
build) + `--seed-dataset` (a wamn-seed dataset compiled against the catalog) +
`--flow` (validate, register, ACTIVATE — deactivating prior versions; the
`flow_id` column is written from the graph's embedded id, so the dispatcher's
column==graph guard holds by construction). Registration REJECTS a webhook
path another active flow of the tenant already serves (any webhook trigger,
sync or async): `register_flow` pre-checks with a named error before any
write, the `flows_active_webhook_path` partial-unique expression index in
`deploy/flows.sql` backstops concurrent registration, and
deactivate-prior + insert run in ONE transaction so a losing race never
strands a flow with no active version. The f1bench gate provisions its
ephemeral schema through the SAME helpers, so the flags are gated too —
including the collision rejection.

## Request lifecycle (D15 sync)

1. **Route** — active flows are re-read per request (the S3 hot-reload
   discipline); the POST path must match an active flow's
   `webhook{sync:true, path}` trigger. Path→flow is unambiguous: registration
   rejects a path another active flow serves (pre-check + unique index,
   wamn-i7i). No match → 404; non-POST → 405. Neither mints a run.
2. **Write-ahead** — one INSERT creates the audit row *before any effect*:
   server-minted `run_id` (`gen_random_uuid()`), `status='dispatched'`,
   `trigger_source='webhook'`, `input_json` = the payload **verbatim** (a
   non-JSON body is carried as a JSON string — it still gets its run and its
   400). Then `dispatched → running`.
3. **Drive** — the 5.2 engine walks `deploy/f1-flow.json` (same topology as the
   5.1 structural fixture, F1-shaped node types):
   `validate` →(main)→ `upsert` → `evaluate` →(main | `out-of-spec`→`holds`)→
   `respond-ok`, with `validate` →(`error`)→ `respond-bad`. Every completed
   node records a `node_runs` row in the flowrunner's 5.7 shape; an errored
   node is recorded as an emission on the **`error` port** carrying the same
   `{"error": {message, code?, data?}}` payload the engine routes — but ONLY
   when the node actually has an error edge. A run-failing node (no error
   edge) leaves no `node_runs` row: reconstruction folds every completed row
   as a routed emission, so an error row for an edge-less node would
   reconstruct a failed run as completed; its record is `runs.fail_*` (the
   flowrunner contract). The taxonomy lands in `error_kind`/`error_detail`.
4. **Answer** — the terminal `respond` node's payload is the body and its
   config the status; `runs` ends `completed` with `result_json` = the body.

## Node semantics (F1-shaped, **not** D8)

These are named, catalog-pinned nodes; the generic raw-SQL `postgres-query`
node lands with 5.3 under the D8 decision (wamn-r13, decided: raw-SQL node
behind a per-project permission flag, default OFF — see the decision table).

- **validate-receipt** — payload shape (unknown keys rejected; decimals as
  exact-decimal STRINGS or JSON integers, JSON floats refused; RFC 3339
  `received_at`; `numeric(p,s)` range checks), then business-key resolution
  (supplier by `suppliers.name`, site by `sites.code`, material by
  `materials.name`) and spec prefetch. Every client fault is `invalid-input`
  → the error edge → 400 with the full issue list.
- **upsert-receipt** — ONE `wamn:postgres` transaction: receipt upsert on the
  tenant-scoped composite natural key (`ON CONFLICT (tenant_id, receipt_no,
  supplier_id) DO UPDATE`), then replace the line set (DELETE + INSERT,
  `RETURNING` ids). Dropping the transaction without commit rolls back (host
  guarantee).
- **evaluate-specs** — pure exact-decimal (scaled-i128, no float anywhere):
  out-of-spec iff measured moisture **strictly exceeds** `moisture_max_pct` or
  `|weight_kg − quantity|` **strictly exceeds** `weight_tolerance_kg`
  (boundary equality is in-spec). All clean → the final `{receipt_id, holds:
  []}` on `main`; otherwise the `out-of-spec` branch.
- **create-holds** — one `quality_holds` row per offending line (`status
  'open'`, `opened_at now()`, the receipt's site; `RETURNING id` feeds the
  response's `hold_id`s), in ONE transaction: a mid-loop transient failure
  rolls back every partial hold, so no orphaned hold's FK can permanently
  block the receipt's replace-lines path.
- **respond** — passthrough that fixes the HTTP status from config via the
  pure `wamn_f1::respond_status` rule. An error-path respond declares the
  error code it answers for (`config.error`); a payload whose `error.code`
  differs (an infrastructure failure routed down the error edge) answers
  **503**, never the configured 400 — and the 503 body is a GENERIC
  `unavailable` message: raw pg-error detail is audit material
  (`node_runs`/`result_json`), never echoed to the untrusted caller.

Failures outside the error edge (DB down mid-upsert, unknown node type) end
the run `failed` (`fail_kind`/`fail_node`/`fail_reason` recorded) → HTTP 500
with a generic body naming only the node (the detailed reason stays in
`runs.fail_reason`).

## Payload / response contract

```jsonc
// POST /receipts
{
  "receipt_no": "r-1001",            // <= 64 chars
  "supplier": "acme",                // suppliers.name
  "site": "hq",                      // sites.code
  "received_at": "2026-07-12T08:00:00Z",
  "lines": [{
    "material": "resin-a",           // materials.name
    "quantity": "100.000",           // numeric(12,3), positive — persisted
    "moisture_pct": "11.20",         // numeric(5,2), measured — trace-only
    "weight_kg": "99.980"            // numeric(12,3), measured — trace-only
  }]
}
// 200
{ "receipt_id": "<uuid>", "holds": [
  { "hold_id": "<uuid>", "line": 1, "material": "resin-a",
    "reason": "moisture 13.10 pct exceeds max 12.50 pct", "status": "open" } ] }
// 400 (any client fault)
{ "error": { "code": "invalid-input", "message": "...", "data": { "issues": [...] } } }
```

Measured values (`moisture_pct`, `weight_kg`) are not persisted on
`receipt_lines` (the catalog has no columns for them); they live in the run
trace (`node_runs.input_json`) and the response.

## Gates

- `cargo test -p wamn-f1` — decimal/payload/evaluate/shape units + two
  drift-guards (SQL identifiers vs the poc-receiving catalog fixture; the
  implemented node set + topology vs `deploy/f1-flow.json`).
- `cargo test -p wamn-host` — fixture coherence (burst = 20 receipts, 3
  out-of-spec, 4 holds) + the schema-rewrite guard.
- **f1bench** (in-cluster Job of record `deploy/f1bench-job.yaml`; local via a
  throwaway PG) — drives `poc_webhook_f1.wasm` in-proc via ProxyPre and
  cross-checks through `api_gateway.wasm` over ONE ephemeral schema. Modes:
  `happy` (sync 200 + write-ahead + 4-node trace + persisted rows), `holds`
  (out-of-spec → response holds + `quality_holds` rows + `out-of-spec`-port
  trace), `invalid` (the malformed set → 400 `invalid-input`, runs still
  audited, validate recorded on the error port; 405/404 mint no runs), `burst`
  (the acceptance script: 20 receipts / 3 out-of-spec / 4 holds + verbatim
  write-ahead payloads + RLS isolation), `rest` (generated REST lists the
  holds incl `expand=line`).
- **f1proof** (`deploy/f1proof-job.yaml`) — the same burst + REST cross-check
  over REAL cluster networking against the deployed workloads
  (`deploy/f1-workloads.yaml`), plus the DB audit; provisioning via
  `deploy/f1-provision-job.yaml` (the `f1-fixtures` ConfigMap).

## v1 limitations (deliberate)

- **ERP retries are new runs.** Each POST mints a fresh run id; re-POSTing an
  out-of-spec receipt opens duplicate holds (there is no natural unique key
  for a hold), and re-POSTing a receipt whose lines already carry holds fails
  the replace-lines transaction on the FK (conservative: lines under hold are
  not silently replaced). Idempotent webhook delivery keys on
  `runs.idempotency_key` when a client supplies one — a 4.6/5.14 refinement.
- **Orphaned sync runs stay `running`.** A host death mid-request leaves the
  write-ahead row in `dispatched`/`running`; the 5.14 janitor only retires
  QUEUED runs. A runs-level sweep for unqueued sync runs is future work
  (pairs with the wamn-fqg.7 wedge policy).
- **Auth is the tenant claim.** The ERP identity/API-key → `app.user_id`/
  `app.role` claims are 4.2 (wamn-0xd); v1 is tenant-scoped like the 4.1
  gateway.
- Flow hot-reload is per-request re-read (one indexed row); the 4.4 doorbell
  refinement applies unchanged when it lands.

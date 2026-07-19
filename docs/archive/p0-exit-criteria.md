> **Archived 2026-07-19** — P0 closed; results live in `docs/p0-results.md`; retained for the go/no-go thresholds that gate re-measurement.

# P0 Exit Criteria — Measurable Go/No-Go per Spike

Numbers, not vibes. Each spike lists: deliverable, measurements, pass thresholds, and what a failure triggers. Bench environment: 3-node kind/k3d cluster on a dev workstation-class machine, in-cluster Postgres (CNPG single instance), results recorded in a `p0-results.md` with raw numbers. Thresholds are dev-cluster values; production SLOs (plan 5.14/D15) are separate.

## S1 — Custom host image (1.3)
**Deliverable:** `wash-runtime`-based host image with `wasi:http`, `wasi:config`, `wamn:postgres` (stub ok initially), `wamn:node/control` registered; OTel on; deployed via the Helm chart with custom image args.
**Measure:** component cold instantiation p50/p99; per-component memory overhead at 100 resident components; memory-cap enforcement (component allocating past 256 MiB is killed, host survives).
**Pass:** instantiation p99 < 10ms; host stable at 100+ resident components; cap kill is clean (no host restart).
**Fail →** revisit pooling-allocator config / density assumptions in the architecture summary before P1.

## S2 — `wamn:postgres` plugin (2.1–2.2)
**Deliverable:** plugin implementing `wamn-postgres.wit` (query/execute/transaction/cursor) with claim injection via `SET LOCAL`.
**Measure:** sustained qps from one component (single-statement queries, 8 params, 10-row results); p50/p99 latency; pool behavior at saturation.
**Pass:** ≥ 2,000 qps sustained single host, p99 < 10ms in-cluster; graceful `connection-unavailable` (not hangs) at pool exhaustion.
**Security gates (all mandatory):**
- **Chaos test:** epoch-kill a component mid-transaction 100×; assert every subsequent checkout is transaction-free and claim-free, and the killed connection was destroyed (pool churn observable), never reused.
- **RLS test:** two workload identities, same table; identity A's query returns zero rows of B's data across 10k randomized attempts; `permission-denied` carries no policy detail.
- **Injection test:** no code path accepts interpolated SQL from params; fuzz params with SQL fragments, assert byte-identical treatment as data.
**Fail →** the platform thesis (safe in-process DB capability) is at risk: stop, redesign the plugin before anything else proceeds.

## S3 — Flow-runner PoC (5.2)
**Deliverable:** minimal runner: loads flow JSON from catalog table, walks a 5-node graph (webhook-in → transform → `wamn:postgres` write → conditional → respond), checkpoints run state.
**Measure:** per-step dispatch overhead (standard node, same-binary call); hot-reload latency (catalog flip + doorbell → new version live); checkpoint/resume (kill runner mid-run, replica resumes).
**Pass:** standard-node dispatch overhead < 50µs p99; hot reload < 1s; resumed run completes with no duplicate side effects (idempotency verified).
**Fail →** dispatch overhead high = re-examine plan representation; resume failure = rework checkpoint granularity before P1's queue work builds on it.

## S4 — Custom-node invocation + config parse (5.6, D7, design-note 9b)
**Deliverable:** one custom node (TS via JCO and Rust variants) behind in-cluster HTTP; a hand-built `wac`-composed equivalent of the same 3-node flow.
**Measure:** HTTP hop overhead p50/p99 (invoke minus node compute); composed vs interpreted total-latency gap on (a) an I/O-bound flow, (b) a compute-bound flow; config JSON parse share of cold dispatch.
**Pass / decision rules:** HTTP hop p50 < 2ms confirms D7. Interpreted-vs-composed gap < 5% on the I/O-bound flow confirms the interpreter default; a large gap on the compute-bound flow is *expected* and merely confirms frozen flows' post-GA slot. Config parse ≤ 5% of cold dispatch closes design-note 9b; above it, revisit.
**Fail →** hop p50 > 5ms: pull the component-linking/wRPC spike forward from "later optimization" to P1.

## S5 — Logging capture (9.3)
**Deliverable:** `wasi:logging` (or stdout capture) in the host emitting OTel logs → Loki.
**Measure:** enrichment correctness (tenant/project/flow/run/node on every record); log loss under burst (10k lines/sec for 30s); pipeline overhead per log call.
**Pass:** 100% enrichment; < 0.1% loss at burst with rate-limit engaging (not silent drops — limits surface as a counter); < 50µs per log call in-guest.
**Fail →** logging becomes a P1 workstream with a buffer/agent redesign; do not ship P1 without it (run history depends on the same enrichment).

## S6 — Test host plugin-swap (11.1)
**Deliverable:** alternate host build: `wamn:postgres` → ephemeral schema from template; `wasi:http` outgoing → recorder/stub; `wasi:clocks` → virtual.
**Measure:** the *same* S3 flow binary (unmodified) passing under the test host; a flow containing a 24h delay node completing under virtual time; recorded egress matching an expectation list.
**Pass:** zero component/runner changes between prod and test hosts; 24h-delay flow completes in < 1s wall time; egress spy catches an intentionally added unexpected call.
**Fail →** the mock-at-capability-boundary thesis is weakened: identify which capability leaked ambient behavior and fix determinism rules (design-note 9) before Epic 11 is planned in earnest.

## Cross-cutting exit
P0 is done when: all six pass or have a written decision from their fail branch; `p0-results.md` records raw numbers; and the following downstream decisions are formally closable with data — D5 (pooling topology, from S2 saturation behavior), D7 (confirmed or escalated, from S4), design-note 9b (from S4), and the proposed dispatch SLOs (sanity-checked against S2+S3 latencies).

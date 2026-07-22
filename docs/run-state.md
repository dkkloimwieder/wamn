# Run state persistence (5.7)

Durable run state is what makes a flow run **traceable and resumable** (the P1
exit criterion): the `runs` / `node_runs` tables, at-least-once execution keyed by
idempotency, a queryable run history, **branch-aware replay** from captured
inputs, and **partial re-run** from a failed node. It is the durable half of what
the pure engine ([`wamn-runner`](flow-runner.md), 5.2) left as an in-memory seam —
5.2 holds a [`RunState`] with a single `step_seq` counter; 5.7 persists one row per
node execution and rebuilds the exact frontier from those rows.

The split mirrors the rest of the platform: a **pure crate** (`crates/wamn-run-store`)
holds the record model and all reconstruction/re-run logic — no DB, no wasm, no
clock, unit-tested off-cluster — and drives two additive **engine primitives**
(`Plan::resume` / `Plan::seed_at`); the **driver** (`components/flowrunner`)
supplies the `wamn:postgres` effects against the schema in
[`deploy/sql/run-state.sql`](../deploy/sql/run-state.sql).

## The tables

`runs` — one row per execution: the flow + version, the lifecycle `status`
(`dispatched`→`running`→`completed`/`failed`/`cancelled`, plus a janitor
`infrastructure-failure`), the trigger `input_json` (what a replay re-runs), the
`result_json`, a transient `state_json` (e.g. a `delay` node's parked-wake), the
`idempotency_key` (at-least-once redelivery dedupe), the lineage links
(`replay_of` / `root_run_id`), and the `fail_kind`/`fail_node`/`fail_reason`
mirrored from the engine `FailKind`.

`node_runs` — one row per node execution, the **reconstruction source**. Its key
`(tenant_id, run_id, node_id, occurrence)` is loop-safe: `occurrence` disambiguates
a node the flow revisits (0 = first visit), while retries of one occurrence share
the row and bump `attempt` — they never create new rows. A completed row carries
`status` (`success`/`error`), the emission (`output_port` + `output_json`), and the
node `input_json` (what a partial re-run seeds). `running`/`parked` rows are
outstanding nodes.

Both tables sit on the house tenant floor — `FORCE ROW LEVEL SECURITY` keyed on
`current_setting('app.tenant', true)`, granted to the non-owner `wamn_app` role —
so a missing claim sees zero rows. `node_runs` foreign-keys `runs`
`ON DELETE CASCADE`.

## SQL builders (single source, SR2)

The `runs`/`node_runs` SQL is written **once**, in `wamn_run_store::sql` — pure
`String` text builders in the house shape: values are always `$n` parameters,
identifiers are pinned, table names are **unqualified** (the host injects the
schema via `search_path`, the S6 schema-as-fixture pattern), the tenant comes from
`current_setting('app.tenant', true)`, and every status literal interpolates from
the `status` model enums so a builder cannot drift from the lifecycle it writes.
The module carries no DB driver, clock, or `tokio` in its dependency closure, so it
is **guest-compilable**: both wasm guests (`flowrunner`, `poc-webhook-f1`) bind
these builders through `wamn:postgres`, while host drivers execute the identical
text through `tokio_postgres`. Whoever holds the connection executes — there is
never a second author of the schema's statements (docs/archive/structure-review.md SR2).
The load-bearing shapes (`ON CONFLICT` idempotency, the `dispatched`→`running`
guard, the deliberately unconditional completion write, the `success`/`error`
reconstruction filter) are pinned by shape unit tests in that module; the runtime
`flowbench`/`failoverbench` gates prove the end-to-end behavior.

## Branch-aware replay (reconstruction)

On every invocation the driver reconstructs the run rather than loading a linear
checkpoint. `wamn_run_store::reconstruct` reads the completed `node_runs` in `seq`
order and folds each — as a `Success { payload, port }` on its recorded port —
through the engine's `Plan::resume`. Because the fold uses the same
`apply`/`enqueue_successors` the original walk used, the rebuilt frontier is
**exactly** what was left outstanding: the same branch was taken, the same merges
arrived, and an **error-routed** node re-enters its error branch (it was recorded
as an emission on the `error` port carrying the `{"error": …}` payload, so
reconstruction needs no error taxonomy). A node with a persisted record is never
re-dispatched — its effect does not repeat.

`occurrence` is engine-computed (`Dispatch::occurrence`: the count of the node's
prior **completed** visits in the run), so any node visited more than once — a
loop, or a **merge**, which runs once per arriving token even in an acyclic flow —
persists one row per visit, and replay walks the history visit-by-visit. Retries
of one visit share its row (`attempt` bumps; `occurrence` advances only on
completion). The old v1 shortcut (`occurrence = 0` always) silently collapsed a
revisited node's history: correct only when **no node is visited more than
once**, a condition merges break even in acyclic flows (wamn-03m / cjv.10 / R24).

A record whose node does not match what the flow dispatches at that point is a
`ResumeError::Mismatch` — a drift guard against a corrupt history or a flow-version
skew. A completed node with no captured emission (9.6 capture off) makes the run
`ReconstructError::CaptureOff` — explicitly non-replayable rather than silently
wrong.

### At-least-once, exactly-once effect

An effectful node runs its effect when it is *outstanding* (no record yet). If the
runner is killed in the window between a node's DB write and its `node_runs` row,
the node is outstanding on resume and re-runs — an at-least-once replay absorbed by
the node's own idempotency (`pg-write`'s `sink` `ON CONFLICT DO NOTHING`), so a
killed-and-resumed run leaves exactly one side effect. This is the kill-mid-run
gate, now flowing through reconstruction rather than `step_seq`.

The same reconstruction is the resume half of **checkpoint/resume on replica loss**
(5.14): when a runner dies, a second replica reclaims the run from the durable queue
(the 5.14 lease-expiry reclaim) and resumes it here — the kill-mid-run guarantee
carried across a replica boundary. See docs/run-queue.md § *Checkpoint/resume on
replica loss*.

## Partial re-run & replay

Both mint a **new** run linked to its origin (`replay_of` + `root_run_id`), leaving
the original run and its node-runs immutable — an audit/billing-safe lineage chain:

- **replay** (`plan_replay`) re-runs the whole flow from the captured trigger
  input; the driver `Plan::start`s the new run.
- **partial re-run** (`plan_partial_rerun`) re-enters a chosen node with *its*
  captured input via `Plan::seed_at`, walking only the downstream subtree —
  already-committed upstream effects are not re-fired. Whether a replayed node
  re-applies its effect is the node's own idempotency concern (5.3), so 5.7
  recomputes from capture by default.

### Resume pins the run's persisted version

A resume reconstructs against the run's **persisted `flow_version`** — the
version stamped on the `runs` row when the run first opened, which the dispatcher
sets to the active version at write-ahead time — not whatever is active now
(wamn-cox). So a flow edited or hot-reloaded mid-run can never make a resume fold
its recorded `node_runs` against a divergent graph: the `components/flowrunner`
driver loads the exact version on every drive path (the direct `execute`, the
unpartitioned claim, and the partitioned claim all pin it), and a hot-reload is
still picked up because newly dispatched runs carry the new version. `Plan::resume`
still raises `Mismatch` as the backstop against a corrupt history. Which version a
*new* run executes is a hot-reload / dispatcher concern (4.4 / 5.14).

## Node-level I/O capture (9.6)

5.7 stores each node's I/O inline; **9.6 (wamn-srb)** decides *how much* and *in
what form*, per flow. Platform credentials are structurally absent (handles, per
contract), but **user data flowing through nodes can still carry secrets**, and
node I/O snapshots are the platform's biggest storage-cost driver — so capture is
a policy, not an always-on faithful copy.

The policy is a per-flow field, [`Flow.capture`](flow-schema.md), that rides
`graph_json` (no new plumbing — the flow is in scope at every `node_runs` write).
It has a **mode** and a **`max-bytes`** size threshold (default 64 KiB). The pure
application logic — secret scrubbing, size truncation, preview/size/hash
derivation — lives in `wamn_run_store::capture` (`capture::derive`), unit-tested
off-cluster and linked by the flowrunner guest, which calls it at each
`record_node_run*` / `record_error*` write *before* the `jsonb()` choke point.
The 9.6 seam columns already reserved on `node_runs` (`preview_head`,
`payload_size`, `payload_hash`, `capture_mode`, `redacted`) are filled there; the
cold-path byte store (`input_ref`/`output_ref`) stays 5.10's and is left null.

**Modes** (`capture_mode` records the *effective* mode per row):

| mode | stored payloads | preview/size/hash | reconstruct | `redacted` |
|---|---|---|---|---|
| `full` (default) | faithful (replayable) | yes (preview scrubbed) | exact | false |
| `scrubbed` | secret-scrubbed | yes | works, replays scrubbed values | **true** |
| `preview` | NULL | yes | **CaptureOff** (non-replayable) | false |
| `off` | NULL | none | **CaptureOff** | false |

The `preview_head` (first 256 chars) is **always** derived from the *scrubbed*
serialization, so the editor's inspection panel never surfaces a raw secret even
under `full`. `payload_size` is the full serialized byte length and `payload_hash`
is a pure `fnv1a64` content id of the output emission.

**Size threshold.** In *any* mode, a payload whose serialization exceeds
`max-bytes` is stored **preview-only** (payload NULL, `capture_mode = 'preview'`)
with its full size/hash retained — v0 truncates in Postgres; 5.10 will move the
bytes to the object store behind `*_ref`.

**Scrubber (v0).** Pure, guest-compilable, no regex: JSON **key-name** redaction
(case-insensitive substring on `password`/`passwd`/`secret`/`token`/`api_key`/
`apikey`/`authorization`/`private_key`/`credential`) replacing the whole value
with `[redacted]`, plus **value-shape** checks on string leaves (`Bearer ` tokens,
`-----BEGIN` PEM blocks, `AKIA` AWS key ids). Recursive over the `Value`; a secret
key's subtree is redacted wholesale (never recursed into). Kept off the `full` hot
path — there it touches only the preview.

**The `scrubbed`/replay tradeoff.** `scrubbed` mode scrubs the **stored**
`input_json`/`output_json` (and the preview) and sets `redacted = true`, so no raw
secret is left in the hot store — but a replay/partial-re-run then replays the
*scrubbed* values, not the originals. `full` keeps replay exact at the cost of
storing the raw payload; `preview`/`off` make the run non-replayable
(`ReconstructError::CaptureOff`) by design. Choose per flow's PII stance.

**Error rows.** The error payload (the routed `{"error": …}` emission) is captured
under the same policy; when the stored payloads are scrubbed the taxonomy
`error_detail` is scrubbed too (it can echo the payload).

**Retention.** The `wamn-ctl prune-run-history` verb (app-role, tenant-scoped)
deletes terminal runs (completed/failed/cancelled/infrastructure-failure) older
than `--retention-days`; `node_runs` cascade via the FK. `cron_anchor` is a
separate table it never touches, so a pruned cron run cannot re-fire its tick
(wamn-fqg.6 — proven by dispatchbench's `retention` mode). v0 is **age-based
only** — replay lineage is not consulted (a lineage-aware policy is a deferral).
Deploy per project-env with `deploy/platform/run-retention.example.yaml`.

**Gate.** `capturebench` (`--mode toggle|truncate|scrub|retention|all`) applies
the real `run-state.sql` to a throwaway schema and drives the same pure capture +
insert builders: toggle (NULL payloads + CaptureOff), truncate (oversized →
preview head/size/hash), scrub (a known secret appears **nowhere** in `node_runs`,
`redacted` set), retention (the real prune verb removes old terminal runs, keeps
recent/non-terminal, leaves `cron_anchor`).

## Scope (5.7) vs. siblings

5.7 owns the run-state schema, at-least-once idempotency, the run-history read
model, branch-aware replay, and partial re-run. It deliberately does **not** own:

| Concern | Owner |
|---|---|
| The durable run queue (`FOR UPDATE SKIP LOCKED`) + leases + NATS doorbell + dispatcher | 5.14 (co-transacts with these INSERTs; owns its own queue table) |
| The node-level I/O **capture policy** (scrub / truncate / per-flow toggle / PII) | 9.6 (fills the `input`/`output`/`preview`/`redacted` slots) |
| The content-addressed **payload byte store** for streamed/large payloads | 5.10 (pointed at by the reserved `*_ref` + `preview_*` columns) |
| Per-node ordering (`strict`/`partitioned`/`unordered`) | 5.11 |
| The `cancel(run, reason)` operation | 5.12 |

The reserved nullable seam columns are where 9.6 and 5.10 land without a schema
change; 5.7 itself leaves them null and stores I/O inline. **9.6 (wamn-srb) now
fills the capture columns** (`preview_head`, `payload_size`, `payload_hash`,
`capture_mode`, `redacted`) per the per-flow policy — see *Node-level I/O capture*
above; the cold-path byte-store pointers (`input_ref`/`output_ref`) remain 5.10's.

## Gates

- **`cargo test -p wamn-run-store`** — the model + reconstruction + re-run, pure:
  linear resume, the **branch-aware kill-mid-branch → resume** proof, error-routed
  reconstruction, capture-off non-replayability, drift detection, `seq`-ordering,
  replay/partial-re-run lineage, and the status/DDL drift guards — all off-cluster.
- **`cargo test -p wamn-runner`** — the `resume` / `seed_at` primitives (branch,
  drift, overrun, partial-subtree).
- **live-apply** (`WAMN_RUN_STORE_PG_URL`) — applies `deploy/sql/run-state.sql` to a
  throwaway Postgres and asserts tenant RLS isolation, the idempotency index, and
  the FK cascade.
- **`flowbench`** (S3) + **`testhostbench`** (S6) — the driver's regression, now
  resuming through reconstruction: dispatch p99 < 50 µs, hot-reload < 1 s,
  kill-mid-run exactly-once, S6 sameness / 24 h-delay-under-virtual-time / egress
  spy. Both pass on the rewired runner in-cluster (the gate of record) and locally.

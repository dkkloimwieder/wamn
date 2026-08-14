# Run state persistence (5.7)

Run state is the durable execution record: admission identity, lifecycle,
queue authority, caller outcome, immutable effect-attempt facts, and bounded
author-facing node history. It does **not** make the flow engine resumable. The
engine executes one in-memory walk and exposes no reconstruction, restore,
replay, or partial-rerun API.

`crates/execution/run-state` owns the pure decisions and parameterized SQL for
the transactionally coupled `runs`, `node_runs`, `run_queue`, lease, caller, and
terminal lifecycle. It contains no database driver, wasm runtime, broker, or
clock. Host and guest adapters execute these statements against
[`deploy/sql/run-state.sql`](../../../deploy/sql/run-state.sql) and
[`deploy/sql/run-queue.sql`](../../../deploy/sql/run-queue.sql).

## Durable records

`runs` is one row per admitted execution. It records the pinned flow and
execution-bundle identity, lifecycle status, trigger input, result/failure,
caller outcome, admission context, and optional capture policy. For CDC event
runs, `event_source_run_id`, `event_root_run_id`, and `event_depth` are retained
causation facts protected by a constraint, immutable trigger, index, and
column-scoped privileges. They are not execution lineage. The former
`replay_of`, `root_run_id`, and `runs_root` objects are retired.

`node_runs` is a bounded history and current occurrence projection. Its key
`(tenant_id, run_id, frame_id, local_node_id, occurrence)` distinguishes loop
visits and framed execution. `seq` preserves history order; capture-enabled runs
may retain scrubbed input/output facts. These rows are not folded back into an
engine frontier and never authorize another effect dispatch.

The immutable effect-attempt ledger is the authority for whether a claimed run
may execute. Capture is independently optional and is never recovery authority.

All run-plane tables use forced tenant RLS keyed by
`current_setting('app.tenant', true)`. The guest-safe application role receives
only the table and column privileges required by the owned transitions.

## Single-shot crash boundary

Execution begins with a fresh `Plan::start` state. There is no `Plan::resume`,
frontier snapshot/restore, history fold, or mid-graph seed.

When a lease expires, the production claimant classifies the durable facts
before doing anything else:

- If no immutable effect attempt exists, the pre-effect claim path may delete
  replaceable node projections, clear transient `state_json`, and restart the
  single-shot walk from zero.
- If any immutable effect attempt exists, the run becomes
  `effect-uncertain`, loses queue eligibility, and is never dispatched again.

This is deliberately conservative. An outcome that may have escaped is not
converted into an assertion of success or failure, and an idempotency hint does
not grant permission to resend it.

Some forward transitions still write `state_json` or node history while the
current claim is live. Those writes support fenced transition composition and
diagnostics; they are not a durable execution checkpoint and have no public
reader/restore contract.

## SQL builders and transitions

`wamn_run_state::sql` is the single source for the basic run/history statements.
Values are `$n` parameters, identifiers are pinned, and run-plane table names
are unqualified so the host-selected `search_path` supplies the project schema.
The builders remain guest-compilable.

`wamn_run_state::transitions` owns the queue-joined executor mutations. Every
executor write verifies the target run, lease owner, and lease generation before
recording a reserved boundary, completing an effect attempt, choosing a caller
outcome, renewing a lease, or terminalizing. `FenceLost` is absolute: the caller
must stop without another store access.

The persisted `flow_version` and immutable execution-bundle identity pin the
single-shot execution to what admission selected. A newer catalog head never
changes an already admitted run.

## Node-level I/O capture (9.6)

Capture is immutable admission policy, carried by `runs.capture_mode` as `full`
or `off`. Published HTTP/event runs and test-set cases are admitted `off`;
direct draft runs may use `full`.

`full` stores scrub-redacted bounded input/output history. Oversized output
stores no body and retains only bounded metadata used to project
`output-too-large`. `off` stores no per-node payload facts. The scrubber is a
known-pattern redaction floor, not a secret-classification guarantee.

Retention is age-based. `wamn-ctl prune-run-history` deletes old terminal runs;
`node_runs` cascades through its foreign key. It does not consult retired rerun
lineage and does not touch scheduler anchors.

## Scope and gates

Run-state owns the run schema, history projection, effect-attempt authority, and
fenced lifecycle statements. The durable queue and its claimant own scheduling;
the payload store owns external bytes; the pure engine owns only the current
in-memory graph walk.

- `cargo test -p wamn-run-state` checks storage vocabularies, SQL shapes,
  capture, and transition decisions off-cluster.
- `cargo test -p wamn-runner` checks the single-shot engine walk, context,
  branching, errors, budgets, and in-memory retries.
- The live run-plane gate applies the schema from zero and exercises populated
  cutovers, RLS, privileges, constraints, exact DDL guards, and rollback.

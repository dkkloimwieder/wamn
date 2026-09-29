# Write log

Updated through: 2026-09-26, `main` at `b32865103`.

## 1. Goal

One platform table per database, `app_system.write_log`, holds the idempotency record of every create and command. It replaces every `*_command` table and every hand-written claim, replay and finalize statement. A retry with the same key and the same request answers the stored result. A retry with the same key and a different request is refused. Update and delete keep `row_version` as their guard and write nothing to the log.

The change removes one table per model and per command from every application schema, and three SQL files from every custom command. It adds no new rule: the retry rule is the one the claim tables enforce today, in one place.

## 2. Fixed rules

- One idempotency record per database, `app_system.write_log`. No application table implements or references the claim.
- The claim and the work commit together or not at all. There is no state between them.
- The key is the field the operation's `idempotency.key` names in its contract (`idempotency_key` for a create, a path such as `value.idempotency_key` for a command). The codec reads it from there. Its scope is the database and the operation.
- Same key, same request: the stored result, no work. Same key, different request: `idempotency_conflict`, no work.
- A refused item leaves no log row. Its key is free again. A retry does the work again, and a different request under that key later is admitted. This is deliberate: a refusal is a function of the request and the state, and the state may have changed.
- A retry that answers a stored result runs no post-commit work. No event publishes twice.
- Update and delete do not claim. `row_version` is their guard, as today.
- An id comes from the insert's `RETURNING`. Nothing pre-mints an id.
- The claim transaction runs under `READ COMMITTED`. The platform's connections run it today as the server default; the codec sets it on `BEGIN` so a session default cannot change it. The retry protocol in §4.2 depends on it.
- No manifest field names a table, a statement or a column for the claim. `idempotent_by: claim` is the whole declaration.
- The three log statements are fixed SQL text, one file each. The generator emits them once per package and lists them in the contract of every claim operation, so every contract carries the same three digests. A change to the text changes every claim operation's contract digest.
- Replay of the log is not a goal of this change. Nothing here may make it impossible later, and nothing here builds toward it.
- The engine's intent record (`crates/platform/engine/src/operation/intent.rs`) stays what it is: the edge's record for an operation with no SQL. It is not extended to the cloud. `wamn-an24` is withdrawn.
- The write log and the engine's intent record are two stores of one rule set. The write log takes its definitions from `intent.rs`: which field is the key (`idempotency.key`), the canonical bytes of the request, the conflict answer `idempotency_conflict`, and the stored-outcome answer. Where the guest can link that code, it calls it; where it cannot, it copies the definition and a generator test pins the two equal. No third definition appears.

## 3. Current state

Measured on `main` at `b32865103` on 2026-09-26.

| Place | Today |
| --- | --- |
| Claim tables | One `<name>_command` table per create and per custom command, in the application migration: `idempotency_key` primary key, `canonical_command bytea`, and one pre-minted id column with `DEFAULT gen_random_uuid()` (`apps/wamn_wms/migrations/0001_initial.sql:8-16`). WMS has 7, Receiving 3, the platform fixture 1, `edge_samples` 1. |
| Generated statements | For each create the generator writes `create_claim.sql`, `create_replay.sql` and `create.sql` (`crates/schema/generator/src/sql.rs:69-106`, `generate/contracts.rs:645-648`). The replay joins the claim table to the model table by the minted id and returns the model row. |
| Custom commands | The author writes the claim, replay and finalize SQL by hand and names them in `wamn.json` under `claim: {table, identities, claim, replay, finalize}` (`apps/wamn_receiving/wamn.json:401-408`). WMS has 12 such files, Receiving 3, the fixture 3, `edge_samples` 2. |
| The flow | The hand-written data access runs it inside one transaction: replay read, claim insert, work, commit; on a lost claim race a second replay read (`apps/wamn_wms/data/src/packaging.rs`). A stored request that differs answers `IdempotencyConflict`. |
| Result on retry | Not stored. The replay re-reads the model row through the join. A custom command whose result is not one row (`record_receipt`) rebuilds it from the claim columns. |
| Statement authority | The host builds one statement set per operation from the served contract's `statements[]` (`crates/execution/host/src/operation.rs:585-596`) and activates that set alone for the invocation (`crates/execution/host/src/operation/native_policy.rs:257-260`). A digest outside the set is `UnknownStatement` (`crates/platform/runtime/src/plugins/wamn_postgres/statements.rs:234-237`). |
| `app_system` | Installed in every project database by `reconcile-run-plane` from `deploy/sql/app-schema.sql`. One project database serves one tenant (`docs/architecture/data-access.md:630`). |
| Edge | The engine's intent rules give an operation with no SQL its record in SQLite. `edge_samples` still has a claim table for `sample.record` on the platform side. |

## 4. Design

### 4.1 The table

`app_system.write_log`, installed by `deploy/sql/app-schema.sql`, so `reconcile-run-plane` puts it in every project database and no application migration names it.

| Column | Type | Rule |
| --- | --- | --- |
| `operation` | `text` | The contract's `operation` without its `@version`, for example `wamn-wms:location/create`. Package-qualified, so two packages in one database do not collide. Version-free, so a retry across a release with the same bytes answers the stored result. |
| `idempotency_key` | `text` | The item's key. Primary key with `operation`. |
| `request` | `bytea` | The canonical bytes of the validated request, without `request_id` and the key. The same bytes the claim tables store today. For a command with a pre-commit participant, the canonical bytes include the pre-commit participation the route selected. Not empty. |
| `result` | `text` | The encoded outcome of the one item, without `request_id` or any other per-call field. Null between claim and finish inside the transaction; never null in a committed row. |
| `created_at` | `timestamptz` | Default `now()`. For retention later. |

The database is the tenant, so the log carries no `tenant_id` and no row policy. `wamn_app` gets `INSERT`, `SELECT` and `UPDATE (result)`, nothing else. No `user_id`: record history stamps the rows the work writes.

### 4.2 The retry

The generated codec of an operation with `idempotent_by: claim` owns the transaction and the claim. The handler receives the open transaction and returns its result. Per item:

1. `BEGIN ISOLATION LEVEL READ COMMITTED`.
2. `INSERT … ON CONFLICT DO NOTHING RETURNING` the claim. When another transaction holds an uncommitted row for the key, Postgres blocks this insert on the index tuple until that transaction commits or rolls back. A committed row makes the insert return nothing. A rolled-back row lets it proceed.
3. Not claimed: read `request` and `result` for the key. The read takes a fresh snapshot, so it sees the row that the insert waited on. Different `request`: the item outcome is `idempotency_conflict`. Same: the stored `result` is the item outcome. Roll back. No post-commit work runs.
4. Claimed: run the handler. On a result, encode the one outcome, `UPDATE … SET result`, commit. On a refusal, roll back: the claim goes with it.

A second caller waits at step 2 for the whole of the first caller's work. A lock or statement timeout there ends the second caller's transaction; its item outcome is the platform's timeout refusal, and the client retries.

Three log statements: `log_claim`, `log_read`, `log_finish`. Their SQL text lives in the generator. The generator emits one copy per package at `generated/sql/write_log/{claim,read,finish}.sql` and lists all three in the contract `statements[]` of every claim operation, so the host's per-operation statement set admits them. One file each, one digest each.

### 4.3 Manifest and generator

- `idempotent_by: claim` is the whole declaration. The `claim` object (`table`, `identities`, `claim`, `replay`, `finalize`) is refused by validation.
- The generator emits no `create_claim.sql` and no `create_replay.sql`. `create.sql` binds no id; the model's `DEFAULT gen_random_uuid()` does the work and `RETURNING` carries it.
- The generator emits `generated/sql/write_log/{claim,read,finish}.sql` once per package, and lists them in every claim operation's contract, generated create and custom command alike.
- A create no longer requires a claim table, which closes that item of `wamn-iowb`.

### 4.4 Custom commands

A custom command keeps its work SQL and loses its claim, replay and finalize files. An id it needs across statements comes from the first insert's `RETURNING`. `record_receipt` inserts the receipt first, takes `receipt_id` from `RETURNING`, and binds it into `insert_receipt_line.sql`. It loses its pre-minted `receipt_id` and the check that the insert kept it.

### 4.5 Data access

The hand-written data access functions stop beginning and committing, and lose the replay read, the claim insert, the race re-read and the conflict compare. They take the transaction and do the work. This is the shrink that pays for the change.

## 5. Issues

One branch, one agent (the routes agent, in place of `wamn-an24`). Each issue lands with its tests. No stop between them.

1. The table and its three statements. `app-schema.sql` gains `write_log` and its grants, idempotent on an existing database. The generator gains the three fixed statement texts. Live test on a disposable database: the claim wins once; the second claim waits on an open transaction and then reads the committed result; a rolled-back claim leaves no row; `wamn_app` can update `result` and no other column.
2. The generator. The codec of a claim operation owns the transaction and the retry; `create_claim.sql` and `create_replay.sql` are not emitted; `generated/sql/write_log/{claim,read,finish}.sql` is emitted once and listed in every claim operation's contract; `create.sql` binds no id; validation refuses the `claim` object. The platform fixture drops `widget_command`, its three claim files and its claim blocks, and its data access takes the transaction. Generator tests on the fixture cover the emitted codec, the three log statements in the contract, and the refusal. One live test through the fixture component on a disposable database: first call; retry with the same bytes answers the same outcome and adds no model row; retry with different bytes answers `idempotency_conflict`; a refused create leaves no log row and a later different request under the same key succeeds; two concurrent calls with one key yield one row and one answer.
3. The applications. WMS, Receiving and `edge_samples` drop every `*_command` table, every claim SQL file and every `claim` object; their data access functions take the transaction and lose the claim flow; `record_receipt` takes `receipt_id` from `RETURNING`. Regenerate; the existing application tests run as they are. No new test of generated code.
4. Closeout. `wamn-iowb` loses its claim-table item. `wamn-an24` closes as withdrawn, with this spec as the reason. The data access and execution pages under `docs/architecture` say the log, not the claim tables, and state the `READ COMMITTED` rule and the no-event-on-retry rule. File the unification finding of section 6 with its transaction-ownership sentence. Workspace test run, log path in the close reason, cluster stages noted pending, merge to main.

## 6. Out of scope

- Replay of the log. It needs a stated purpose first, then commit order, platform-minted values and a determinism rule. Nothing in the table shape blocks it.
- Retention. One row per write, bounded by the request size, and the table grows without bound until a retention period comes with the record-history verb.
- Stored refusals. Prior art stores and replays errors; this design frees the key instead (§2).
- The edge box. Its SQL-free commands keep the engine's intent record in SQLite. `edge_samples` on the platform side is in scope.
- An upgrade of a deployed database. The kind clusters are disposable, and the Google Cloud dev database is provisioned again after this change.

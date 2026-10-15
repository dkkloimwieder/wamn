# Host-owned transaction

Updated through: 2026-09-29, `main` at `a0a58f163`. Accepted with the owner rulings of section 7.

## 1. Goal

One code path runs the idempotency rules of every operation. The engine's intent rules run around every call, in the cloud and on the edge. An operation with SQL claims its key in the transaction of its work. An operation without SQL claims in two steps, begin and finish. In the cloud both claims use `app_system.write_log`. On the edge the store stays SQLite.

Today the two paths are separate code (`wamn-4afx`). The generated codec of each claim operation begins its own transaction and runs `log_claim`, `log_read` and `log_finish` (12 codecs). The engine runs `invoke_logged` only on the edge (`services/edge/src/delivery.rs:161-186`). The two stores define the request bytes, the conflict detail and the stored outcome differently.

After this change the host begins the transaction of an operation, the guest works in it, and the host commits it. The engine decides claim, replay and conflict once for every package. The codecs lose their claim code.

## 2. Fixed rules

- The host owns the transaction of an operation that declares a connection and logs intent (create, update, delete, command). The host begins it before the guest call and commits or rolls it back after the call. The guest cannot begin, commit or roll back that transaction.
- One host transaction per item, and one guest call per item, in the cloud and on the edge. The item is the unit of idempotency, so its commit and its uncertainty are its own. No savepoints. The engine loops over the items. The guest gets a one-item list, and the contract does not change.
- A read keeps the implicit transaction of `run` and logs nothing, as today.
- The engine runs the intent rules of `crates/platform/engine/src/operation/intent.rs` around every logging call. No codec, data function or component spells a claim.
- An operation with SQL claims its key inside the host transaction. The claim and the work commit together or not at all, as `docs/plan/write-log.md` section 2 requires.
- An operation without SQL claims in two steps, begin and finish. A begun claim that never finishes is `intent-uncertain`, as today on the edge.
- A commit whose outcome the host cannot know is `intent-uncertain` in the cloud too. That is only the case when the host sent `COMMIT` and did not read its answer. A failure before that is a certain rollback and answers as today (`retry`, `timeout` or `internal_error`).
- The transaction runs under `READ COMMITTED`. The host already sets it (`crates/platform/runtime/src/plugins/wamn_postgres/claims/transactions.rs:132`).
- One definition of each rule: the key field, the request bytes, the conflict answer and the stored outcome. `intent.rs` owns them, and both stores use them.
- The request bytes are the SHA-256 of the canonical item without `request_id`, with the key kept and the participation intent added. `write_log.request` stores that hash, not the raw bytes.
- The conflict detail is `{field, intent}`.
- A refused item frees its key in both stores. The edge store keeps no failure outcome.
- A composed participant works in the base's host transaction through the existing view lease (`transaction_views.rs`). Nothing about participation changes except who begins and commits.
- Post-commit work stays on the event path. The host still stamps causation in its `BEGIN` batch, and CDC reads committed transactions.

## 3. Current state

Measured on `main` at `af16b2b97` on 2026-09-29.

| Place | Today |
| --- | --- |
| Intent rules | `invoke_logged` (`intent.rs:161-294`): begin one intent per item, run the new items as one batch, finish each item. A trap or deadline leaves the intents begun, which is uncertain. `logs_intent` (`intent.rs:63-71`) selects create, update, delete and command. |
| Intent store | Trait `IntentStore` (`crates/execution/run-state/src/intent_store.rs:15-24`). Its only implementation is SQLite (`crates/execution/run-state-sqlite/src/lib.rs`). `begin` commits before the export runs. |
| Callers | The edge passes an intent context. The cloud route (`crates/execution/host/src/route.rs:140`) and the router driver (`router_driver.rs:1013`) pass `None`. |
| Write log | The emitted codec begins a guest transaction, claims, compares the stored bytes, runs the handler, finishes and commits (`apps/platform_fixture/generated/wit/widget_create_codec.rs:249-298`). 12 codecs: WMS 7, Receiving 2, `edge_samples` 1, the fixture 2. |
| Guest transaction | `Connection::begin` returns a `Transaction` that the host holds in the guest resource table (`resources.rs:1251-1290`). A guest can open any number of them. |
| Data access | 41 data functions take `&mut Connection`, 13 take `&mut Transaction` and 1 takes `&mut TransactionView`. Authored data code begins its own transactions in 7 places. |
| Host hooks | `native_policy.rs` `activate` (`:144-303`) binds the invocation scope, the statement set, the transaction scope and the selected participant before the guest call. `revoke` (`:324-337`) runs after it. |
| Outcome | The cloud has no committed-versus-uncertain split. `settle_route` (`router_delivery.rs:466-510`) maps a trap or host failure to `ExecutionFailed`. |

The two stores differ in three definitions:

| Definition | Intent record | Write log |
| --- | --- | --- |
| Request bytes | SHA-256 of the item without `request_id`, with the key kept (`intent.rs:115-121`). | The canonical bytes without `request_id` and the key, with the participation intent added. |
| Conflict detail | `{field, intent}` | `{field}` |
| Stored outcome | A success or a failure (`StoredOutcome`). | A success only. A refusal frees the key (owner ruling in `write-log.md` section 2). |

## 4. Design

### 4.1 The transaction

For an operation that declares a connection and logs intent, `activate` begins one host transaction for each item on a pooled connection, in the same `BEGIN` batch as today. The statements plugin binds it to the invocation scope as the operation transaction. After the guest call, the engine asks the host to finish it: commit when the claims say so, else roll back. `revoke` then drops the binding.

The guest reaches the operation transaction through `statements`. `begin` inside such an operation refuses. That is a break of the `wamn:postgres` WIT, so it is version 0.3.0, and a component built against 0.2.0 does not fit. A data function takes `&mut Transaction` in place of `&mut Connection` and loses every `begin` and `commit`.

### 4.2 The claim

`invoke_logged` takes a store that knows the host transaction. It runs each item alone. For an operation with SQL, the steps for each item run inside the host transaction of that item:

1. Claim the key (`log_claim`).
2. When the key was claimed before, read the stored row (`log_read`). A different request hash answers `idempotency_conflict`. The same hash answers the stored result.
3. When the key is new, the guest call runs with a one-item list. A result finishes the item (`log_finish`). A refusal rolls back the transaction, so its claim goes too.

The edge's batch of new items goes. Its store already claims each item alone.

For an operation without SQL, the store is the existing two-step store. On the edge it is SQLite. In the cloud it is the `write_log` of the package's database, on a host connection of its own.

### 4.3 The write log

`write_log` gains the state of a claim that begins and finishes in two commits: `result` stays null in a committed row until the finish. Today a committed row never has a null `result`. A row with a null `result` is a begun claim, and after a lost finish it is uncertain.

### 4.4 The codec

The claim codec, `write_log_codec.rs` and the three statement files leave the guest. The statements move to the host, which the host's statement set already admits. The generated contract keeps `idempotency.key`, which the engine reads from the published route.

## 5. Issues

One branch, one agent (the routes agent). Each issue lands with its tests. Only workspace tests run. The cluster stages are noted as pending.

1. One definition set. `intent.rs` takes the definitions of section 2: the request hash, the conflict detail and no stored refusal. The engine calls the guest once per item. The edge store drops `StoredOutcome` failures, and its tests follow. A test pins both stores to them.
2. The host transaction. `wamn:postgres` 0.3.0: the statements plugin gains the operation transaction, `begin` refuses inside it, and the engine can commit or roll it back. 0.2.0 leaves the tree. Live test on a disposable database: commit, rollback, a guest `begin` refusal, and a participant view in the same transaction.
3. The write log store. A Postgres `IntentStore` over `write_log`, in the host transaction and in two steps. The same live tests as `write-log.md` issue 1, run through the engine: first claim, retry with the same bytes, retry with different bytes, a refused item, and two concurrent calls.
4. The cloud callers pass an intent context. The route and the router driver build it from the published route.
5. The generator and the applications. Codecs lose the claim, and data functions take `&mut Transaction` and lose `begin` and `commit`. Every package regenerates against 0.3.0 and rebuilds. The application tests run as they are.
6. Closeout. `docs/architecture` states one claim path. `write-log.md` section 6 loses its finding. Workspace test run, cluster stages noted pending, merge to main.

## 6. Out of scope

- Replay of the log, retention, and logging reads. `write-log.md` section 6 keeps them out.
- The legacy `client` interface transaction (`package.wit:91-114`).
- An upgrade of a deployed database. The environments are provisioned again, as for the write log.
- `wamn:postgres` 0.1.0.

## 7. Owner rulings

The owner answered the review questions on 2026-09-29.

1. One host transaction per item, and one guest call per item. No savepoints (section 2).
2. The request bytes are the hash of `intent.rs`, with the participation intent added. The conflict detail is `{field, intent}`. A refused item frees the key in both stores.
3. `wamn:postgres` 0.3.0. 0.2.0 leaves the tree in issue 2, and every package regenerates in issue 5. 0.1.0 stays out of scope.
4. `intent-uncertain` only when `COMMIT` was sent and its answer was not read.
5. `canonical_json_bytes` sorts keys itself, so canonical bytes do not depend on which crates a build joins (`wamn-g1jd`, `a0a58f163`).

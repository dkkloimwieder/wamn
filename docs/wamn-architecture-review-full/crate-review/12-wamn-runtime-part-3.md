# 12 wamn-runtime, part 3: SQL calls, operations, write log

Nothing was built or run. Every statement is from reading the code unless marked *inference*.

## What was read

| File (under `crates/platform/runtime/src/plugins/wamn_postgres/`) | Lines without tests |
| --- | --- |
| `resources.rs` | 1,822 |
| `operation.rs` | 803 |
| `operation_transaction.rs` | 383 |
| `write_log.rs` | 493 |
| `types.rs` | 264 |

Callers were searched across `crates/`, `services/`, `apps/`, `tests/`, `test-support/` and `wit/`.

## One purpose each

| File | Purpose |
| --- | --- |
| `resources.rs` | Host side of the two WIT interfaces `client` (raw SQL) and `statements` (admitted SQL) |
| `operation.rs` | Host side of `wamn:operation`: authorize, begin, commit, roll back one operation |
| `operation_transaction.rs` | The transaction the host owns for one operation |
| `write_log.rs` | The idempotency record in `platform.write_log` |
| `types.rs` | Value and error mapping between Postgres and WIT |

## Findings, each with an option

### Dead code

**1. The raw SQL interface has no caller.**

- `wamn:postgres/client` is imported by two components. One is the `sqlx-command` fixture, already ruled out (tab 04). The other is the materializer (`apps/platform/execution/materializer/wit/world.wit`). Its `src/main.rs` never calls it.
- No component calls `open-cursor` or `fetch`.
- No control, engine or host code produces a named `client` import with a `project` setting (`mod.rs::NamedProject`).
- The one future caller is application dynamic SQL, which is read only (ruled).
- Option A: `client` keeps `query` only. These go: `execute`, `begin`, the transaction resource, the cursor resource, the named `client` variant, `begin_transaction`, `run_execute`, `finish_txn`, `OneShotResult`, `PgCursor`, `NamedProject`, `retained_transaction_for_test`. The materializer drops the import. Estimate: 750 lines in `resources.rs`, 40 in `mod.rs`, plus the WIT text.
- Option B: keep `query` and the read cursor for large results. The rest goes as in A.
- Effect of A on the COMMIT problem (tab 11, finding 21: an application sends COMMIT as SQL text, ends the host transaction, and its next statement runs outside it). With A there is no transaction resource. Each query is one call on its own connection checkout, so a `COMMIT` in text has no later statement to affect.
- Effect on `claims/pools.rs::checkout_workload`: its materializer branch has no caller. The materializer setting is still read by `event_batch.rs`, so the setting stays.

**2. The counter and `platform.` text lists have no raw SQL left to check.** This answers the open point in tab 11. After finding 1 the only path that still runs them is admitted statements (`resources.rs::run_verified_query`). Admission already refuses the same constructs on the parsed SQL. The comment gives two reasons to keep them: a release admitted before that check, and a forged descriptor.

- Option A: delete both lists and `claims.rs::normalized_sql` (about 90 lines). Admission is the one owner. The repository rule is to refuse old rows.
- Option B: keep them for admitted statements as a second check.

**3. Three trait methods are stubs on the Postgres write log.** `IntentStore::uncertain` and `resolve` have callers only in `services/edge`, on the SQLite store. On `OperationTransaction` both are stubs, and `release` is a stub too. On `WriteLogStore`, `resolve` always fails, and `uncertain` with `UNCERTAIN_SQL` has no caller.

- Option: the trait the engine uses has `begin`, `finish`, `release`. `uncertain` and `resolve` stay on the SQLite type. This fits the tab 09 ruling that the intent record joins `wamn-run-state-sqlite`.

**4. A comment names files that do not exist.** `resources.rs::QUERY_DURATION_MS` says the metric is frozen because of `tests/integration/src/metricbench.rs` and `docs/archive/observability/dashboards.md`. Neither exists.

- Option: delete the comment. The metric name stays; `test-support/infrastructure/telemetry.rs` reads it.

### Single truth

**5. The idempotency claim is written twice.** `write_log.rs::begin_with` and `finish_with` serve the engine (`crates/platform/engine/src/operation/intent.rs`). `write_log.rs::begin_opaque` and `finish_opaque` serve `wamn:operation` (`operation.rs::begin`). Both do the same steps: insert the claim, read the row on conflict, compare the request bytes, return new, finished, uncertain or conflict. One parses the result as JSON, the other returns text.

- Option: one claim function that returns the stored bytes and text. Each caller maps the result.
- Open: whether both callers stay. Two paths start an operation today: the host starts it (`execution/host/src/route.rs`, `execution/workflow/src/router_driver.rs`) and the entry component starts it (`wamn:operation`). Decided when the engine and `execution/host` are read.

**6. The SQL value type is one list held in two enums and mapped in two places.** `wamn_catalog::ComponentSqlValueType` and `statements.rs::StatementValueType` have the same ten values. `operation.rs::lower_statements` maps one to the other. `execution/host/src/operation.rs::lower_statement_value_type` maps them again.

- Option: one enum. Its home is `wamn-scalar` (ruled in tab 04). Both mapping functions go.

**7. One operation's facts are spread over five maps.** `operation.rs::OperationWorkload` has `operations`, `grants`, `schemas`, `digests`, `applications`, each keyed by operation id. `operation.rs::begin` indexes two of them with `[...]`, which stops the host if the maps disagree.

- Option: one map from operation id to one struct.

**8. Commit, rollback and finish-read end with the same fifteen lines.** `operation.rs`: record the counter, mark settled, take the trace, drop, restore the component id and the trace. Written three times.

- Option: one function.

**9. The delivery counters are defined twice.** `wamn.router.delivery.attempts` and `.errors` are literals in `operation.rs::OperationMetrics` and constants in `execution/host/src/router_delivery.rs`.

- Option: `wamn-engine` owns the names next to `ROUTER_DELIVERY_ID`, which both already import.

**10. Statement helpers in `resources.rs` come in pairs.**

- `finish_txn` and `finish_statement_txn`: same body.
- `begin_transaction` and `prepare_statement_transaction`: same body up to the moment claims are sent.
- `run_query` and `run_verified_query`: same row loop.
- The three lines that fetch plugin, component id and trace: written twelve times.
- Every `statements` function exists once plain and once for named imports. These already share a body; only the `client` ones did not.
- Option: with finding 1 the first of each pair goes. One helper for the three lines.

**11. `WAL position` is read by the same SQL text in four places.** `claims.rs::model_version`, `claims/transactions.rs::one_shot_statement`, `operation_transaction.rs::wal_position`, `run_stream.rs` (deleted by part 1).

- Option: one constant and one function.

**12. A type that holds a yes or no.** `resources.rs::StatementRunDisposition` has two values and is computed as `result.is_ok()`.

- Option: delete the type; use the boolean.

**13. A UUID is formatted by hand.** `types.rs::SqlCell::from_sql` builds the text of a `uuid` with a hex table. The `uuid` crate is already a dependency (`operation.rs` uses it).

- Option: `uuid::Uuid::from_slice(raw)` and its hyphenated form.

### Single responsibility

**14. Write log SQL is public and tests in two applications run it themselves.** `write_log.rs::CLAIM_SQL`, `READ_SQL`, `FINISH_SQL` are `pub`. Users: `apps/rest_connector/tests/publication.rs` and `apps/wamn_receiving/tests/receiving_data_access.rs`. They do by hand what the host does.

- Option: make the three private. The test parts that run them go (repository rule: test code that repeats the product is deleted).

**15. `operation.rs` is a second purpose inside the Postgres plugin.** It holds the release facts of every operation, the grant check, the caller check, the trace swap and the delivery counters. Only the transaction and the write log are Postgres work.

- Option A: leave it. It is the only host of `wamn:operation` and it needs the plugin's private state.
- Option B: move it out as its own plugin module that calls the Postgres plugin through four functions it already uses (`bind_session_claims`, `begin_operation_transaction`, `bind_operation_transaction`, statement binding).
- No recommendation before the engine and `execution/host` are read; finding 5 may remove one of the two paths.

## Tests that go or change

**Delete**

| Test | File | Goes with |
| --- | --- | --- |
| `cancelled_client_transaction_destroys_its_connection_and_completion_repools` | `resources.rs` | 1 |
| `explicit_statement_run_restores_only_after_success` | `resources.rs` | 12 |
| The test that calls `retained_transaction_for_test` | `execution/host/src/operation/native_policy/tests/authenticated.rs` | 1. Body not read; confirmed in the `execution/host` part. |
| Rows for `open_cursor`, and the rows that compare source text of `resources.rs` with deleted documents | `tests/conformance/tests/effect_spans.rs` | 1, 4. Also against test rule 2 of the repository. |
| The parts that run `CLAIM_SQL`, `READ_SQL`, `FINISH_SQL` | `apps/rest_connector/tests/publication.rs`, `apps/wamn_receiving/tests/receiving_data_access.rs` | 14 |
| `guard_rejects_guest_counter_function_calls`, `guard_rejects_guest_platform_writes`, `guard_allows_platform_reads_and_application_writes` | `claims/tests.rs` | 2, if option A |

**Change or move**

| Test | File | Why |
| --- | --- | --- |
| `the_two_step_write_log_keeps_the_intent_rules` | `write_log/tests.rs` | 3. It runs the shared cases from `wamn_run_state::intent_cases`, which move (tab 09). |
| `model_versions_follow_wal_without_a_counter` | `write_log/tests.rs` | Tests `model_version`. Moves with it (tab 11). |
| `the_host_owns_the_operation_transaction_and_the_guest_works_in_it` | `operation_transaction/tests.rs` | Names `run-stream`. |
| `binding_config_admits_only_the_exact_materializer_authority` | `mod.rs` | 1. `checkout_workload` loses a branch. |

The other tests in `operation/tests.rs` (24), `write_log/tests.rs` (3) and `types.rs` (6) stay.

## Not read

- Test bodies, except where a row above says so.
- `crates/platform/engine/src/operation/intent.rs` beyond the lines that pick the store.
- `statements.rs`, `numbering.rs`: part 4.

## Rulings (2026-11-11)

- Finding 1: **option A.** `wamn:postgres/client` keeps `query` only. An application that needs more than one page uses keyset paging in its own SQL. The host holds nothing between calls.
- Waiting: finding 2 (A or B), findings 3, 4, 6 to 14 (accept or hold), findings 5 and 15 (hold until the engine part).

* Finding 2: **option A.** Both text lists and `claims.rs::normalized_sql` go, with their three tests.
* Findings 3, 4, 6 to 14: accepted.
* Findings 5 and 15: owner asked for the facts first. Below.

## Findings 5 and 15: the two ways to start an operation

Read for this: `crates/platform/engine/src/operation/intent.rs` (non-test part), `crates/execution/host/src/route.rs`, `apps/platform/ingress/http-route/src/guest.rs` and `entry.rs` (the lines that choose and run the path), `crates/platform/engine/src/flow_http_routing.rs` (the lines that set `sealed_operation_id`), `wit/wamn-operation-0.1/package.wit`.

**Who uses which path (fact)**

| Path | Code | Used by |
| --- | --- | --- |
| Entry starts it | `http-route/src/entry.rs::execute` calls `wamn:operation`. Host side: `wamn_postgres/operation.rs`, `write_log.rs::begin_opaque`. | Every authored HTTP route, except routes that sign file results. Chosen in `http-route/src/guest.rs::deliver` when `sealed_operation_id` is set. |
| Host starts it | `engine/src/operation/intent.rs::invoke_logged`. Store: `write_log.rs::begin_with` through `IntentStore`. | Workflow nodes (`execution/workflow/src/router_driver.rs`), the edge service with SQLite (`services/edge/src/delivery.rs`), and HTTP routes that sign file results (`execution/host/src/route.rs`). |

Both paths are in use. Neither can be deleted today.

**What is written twice because of it (fact)**

| Step | Entry path | Host path |
| --- | --- | --- |
| Loop over request elements, key lookup, repeat check, merge answers | `entry.rs::execute` | `intent.rs::invoke_logged`, `request_elements` |
| Request hash | `entry.rs` (hash in the component) | `intent.rs::request_element_hash` |
| Respelling before the hash | `entry.rs::canonicalize` | `RouteCanonicalization::apply` (both ruled out in tab 01) |
| Claim in `platform.write_log` | `write_log.rs::begin_opaque` | `write_log.rs::begin_with` |
| Record id text `operation#key`, and the cut at `@` | `entry.rs` | `write_log.rs::intent_id`, `log_operation` |
| Error words `idempotency_conflict`, `intent-uncertain`, `intent-resolved` | `entry.rs`, `entry_response.rs` | `intent.rs` constants |
| Weak ETag for reads | `entry.rs::list_etag` | `execution/host/src/router_delivery.rs` (noted in tab 01) |

**Options for finding 5**

- **Option A, in this plugin only.** One claim function in `write_log.rs`. It returns new, finished with the stored text, uncertain, or conflict. `begin_opaque` and `begin_with` both call it. About 70 lines go. Both paths stay as they are.
- **Option B, also move the hash to the host.** `begin-operation` takes the request element, not a hash. The host computes the hash with the one function the host path already uses. This is how the tab 01 ruling reads (the server hashes the parsed request). `entry.rs` loses the hash and respelling code. Changes the WIT of `wamn:operation`.
- **Option C, one path for HTTP routes.** Routes that sign file results also use the entry path. Then `execution/host/src/route.rs` and the route ETag code in `router_delivery.rs` go, and the host path serves workflow nodes and the edge only. Not read: the signing code. The comment in `flow_http_routing.rs` says the result must cross the host to be signed, so this needs a new step. Recorded as a question for the `execution/host` tab, not proposed here.

A and B can both be taken. The rest of the table (loop, error words, ETag) belongs to the engine and `http-route` tabs.

**Finding 15, with these facts**

`operation.rs` is the only host of `wamn:operation` and every authored HTTP route goes through it. It uses private state of the plugin: claim maps, statement sets, the operation transaction.

- **Option A:** leave it in the plugin as its own module. Take findings 7, 8, 9 (accepted) to shrink it.
- **Option B:** move it out. It would call the plugin through about six functions that become public.

B adds public surface and removes nothing.

## Carried to other tabs

- Engine and `http-route`: the request element loop and the error words exist twice (table above).
- `execution/host`: can routes that sign file results use the entry path (option C).

## Rulings on 5 and 15 (2026-11-11)

- Finding 5: **options A and B.** One claim function in `write_log.rs`. `begin-operation` takes the request element and the host computes the hash with one function. Check at change: `services/edge/src/operation.rs` also hosts `wamn:operation`.
- Finding 15: **option A.** `operation.rs` stays in the plugin as its own module.
- Tab 12 is fully ruled.

# 13 wamn-runtime, part 4: admitted statements and numbering

Nothing was built or run. Every statement is from reading the code unless marked *inference*.

## What was read

| File (under `crates/platform/runtime/src/plugins/wamn_postgres/`) | Lines without tests |
| --- | --- |
| `statements.rs` | 553 |
| `numbering.rs` | 532 |
| `numbering/set.rs` | 343 |

Also read for callers: `crates/execution/host/src/application_route.rs` (the counter set route) and `operation/native_policy.rs` (the statement binding lines).

## One purpose each

| File | Purpose |
| --- | --- |
| `statements.rs` | Which admitted statements a call may run, and the check that binds and columns match the admitted contract |
| `numbering.rs` | Render the next or current document number of a sequence |
| `numbering/set.rs` | Set one counter for the host route `number_counter.set` |

## Findings, each with an option

### Single truth

**1. Statement authority takes three calls and three maps to reach one state.** `statements.rs::StatementScopes` has `bindings`, `members` and `active`. Both production callers make the same three calls in a row: `bind_prepared_statement_members`, `bind_prepared_statement_operation`, `activate_statement_operation` (`operation.rs::begin`, `execution/host/src/operation/native_policy.rs`). Each binds one operation and activates it at once. Only `active` is read afterwards.

- Option: one call, `activate(scope, statements, members)`, and one map. `bindings`, `members`, the two rebind checks and `bind_statement_operation` (tests only) go. Estimate: 110 lines.
- With tab 11 finding 2 the active set can be a field of the one record per scope.

**2. The list of Postgres type names is written twice.** `statements.rs::statement_type_for_postgres` maps a type name to a value type for the contract check. `types.rs::SqlCell::from_sql` matches the same names to decode a value. A name added to one and not the other decodes a value the check then refuses, or the other way round.

- Option: one function from type name to value type. The decoder calls it first and then handles its sub-cases (`int2` and `int4`, `json` and `jsonb`).

**3. Rules of the number pattern live in the plugin, not in the pattern crate.** `numbering.rs::counter_order`, `date_formats`, `render_counter` and `key_values` use no connection. They state how a pattern is ordered, which dates it reads, how a counter prints, and which keys it needs. `wamn-number-pattern` is the owner of the pattern (tab 08).

- Option: move the four to `wamn-number-pattern`. The plugin keeps the SQL and the loop. About 90 lines move.

**4. The date format order is held in two places that must agree.** `numbering.rs::DATE_SQL` writes `'YYYY|YY|MM|DD|IW|Q'`. The code then splits on `|` and picks by position in `wamn_number_pattern::DateFormat::ALL`.

- Option: each `DateFormat` gives its Postgres token, and the SQL text is built from `ALL`.

**5. The counter set starts an operation by hand. It is the third place that does.** `numbering/set.rs::set_number_counter` and `set_in_transaction` make a scope name, bind claims, begin the operation transaction, claim the request id in the write log, run one statement, commit, and revoke. `operation.rs::begin` and the engine do the same steps for routes. About 110 lines of the file are these steps; the set itself is 45 lines.

- Fact: every other host route write uses `claims.rs::administration_transaction` and has no write log entry.
- Fact: the set hashes its request with its own code (`Sha256::digest(serde_json::to_vec(set))`).
- Option A: one helper in the plugin, "run this work as a logged host operation". The set and `operation.rs::begin` both use it. The hash uses the one host function (tab 12 finding 5, option B).
- Option B: the set uses `administration_transaction` like the other host routes and drops the write log. *Inference:* a repeated set would then rewind a counter that moved in between. The model's unique constraint would catch a reused number, but the caller would see errors.
- Not read: grants. `platform.set_counter` is granted to `wamn_app` only (`deploy/sql/app-schema.sql`), so option B also needs a grant change.

**6. SQLSTATE codes are string literals in six files.** `numbering.rs` (4), `operation_transaction.rs` (2), `claims.rs` (`WAMN0`, `55000`), `resources.rs` (`WAMN2`), `types.rs` (`WAMN1`, `XX000`), `write_log.rs` (`P0002`, also a constant in `numbering.rs`).

- Option: one module of codes in the plugin. This is where the tab 04 ruling (platform errors defined once) lands on the host side.

### Complexity

**7. Every numbered insert in an application with a time zone costs two extra round trips.** `numbering.rs::read_dates` wraps the date query in `SAVEPOINT` and `RELEASE`, because an unknown zone name fails the statement and would end the operation transaction. Three SQL constants and 40 lines serve this.

- Option: check the zone when the `timezone` setting is written. The read then needs no savepoint.
- Not found: the code that writes `platform.setting`. If nothing checks the value on write today, the check is new work there. Carried to the tab that owns settings.

### Defect

**8. Cleanup by workload matches on a text prefix.** `statements.rs::clear_statement_bindings` and `claims.rs::clear_component_claims` keep an entry when `!id.starts_with(workload_id)`.

- *Inference:* unbinding a workload whose id is the start of another id also clears the other one's claims and statements. A cleared claim refuses its calls; it does not leak.
- `statements.rs::clear_statement_scope` already matches the exact id, and has a test for the prefix case.
- Option: match the exact workload id plus its separator, or key the one map (tab 11 finding 2) by workload first.
- Not read: how wash-runtime forms component ids. If ids have a fixed length, the case cannot occur.

## Nothing to remove

- The contract checks (`resolve_statement`, `validate_prepared_statement`, `validate_statement_result`) each have one caller path and no second copy.
- `numbering.rs::render` and `counter_value` have no second copy. The SQLite side has no numbering.

## Tests that go or change

| Test | File | Why |
| --- | --- | --- |
| `activation_exposes_only_the_selected_operation_and_revoke_closes_it`, `each_named_import_runs_only_its_member_set_while_the_operation_is_active`, `exact_scope_cleanup_preserves_a_scope_with_the_same_prefix` | `statements.rs` | 1. Change to the one call. |
| Two sites that call `bind_statement_operation` | `operation_transaction/tests.rs`, `claims/tests.rs` | 1. Change. |
| `a_counter_renders_padded_and_never_truncated`, `counters_take_name_order_after_the_counter_their_key_names`, `the_keys_carry_every_value_the_pattern_names_as_text`, `the_date_formats_name_every_date_token_and_date_key` | `numbering/tests.rs` | 3. Move to `wamn-number-pattern`. |
| `live_an_invalid_timezone_setting_is_refused` | `numbering/tests/live.rs` | 7. Moves to where the setting is written, if accepted. |
| `live_set_counter_is_logged_by_its_request_id` | `numbering/tests/live.rs` | 5. Goes only with option B. |

No test is deleted outright by this part. The other 3 tests in `statements.rs` and 15 in `numbering/tests/live.rs` stay.

## Not read

- Test bodies.
- The code that writes `platform.setting`.
- How component ids are formed in wash-runtime.

## Finding 7, more facts (2026-11-11)

The owner's rule: stored datetimes are UTC. That holds. The setting only decides which calendar day a printed number belongs to.

Read after the first report: `crates/identity/platform/src/configuration.rs`, `deploy/sql/app-schema.sql` (the `platform.setting` table).

- One function writes `platform.setting`: `configuration.rs::write_configuration`, with `SETTING_SQL`.
- No production code builds a `configuration.rs::Setting` value. The one caller of `write_configuration` (`connection_projection.rs`) passes connections and integrations only.
- So no product path can set `timezone` today. Only tests insert the row. Every number renders in UTC in practice.
- The table has no check on the value.

**Options, restated**

- **B1.** Keep the read in `numbering.rs`. Delete the savepoint code. Add the zone check to `write_configuration`, the one writer. That writer has no caller for settings yet, so the check guards a path nobody uses today.
- **B2.** Delete the read, the `Timezone` type and the savepoint code now (about 70 lines and one test). The local day comes back, with the check, when a settings write exists. `Setting` and `SETTING_SQL` are then also without a user and are judged in the identity tab.

Ruled 2026-11-11: B1. The owner states that time zone settings are needed. The read in numbering.rs stays. The savepoint code and its three SQL constants go. The zone check goes into configuration.rs::write\_configuration, the one writer. Open fact for the identity tab: that writer has no caller that passes settings yet.

## Rulings (2026-11-11)

- Findings 1 to 4 and 6: accepted.
- Finding 5: **option A.** One helper for a logged host operation. The write log entry stays.
- Finding 8: owner asked for the facts first. Below.

## Finding 8, researched: the first report had it the wrong way round

Read: wash-runtime at the pinned tag v2.11.0 (`src/engine/workload.rs`, `src/plugin/wasmcloud_postgres/mod.rs`, `src/plugin/wasi_config/mod.rs`), `wamn_postgres/mod.rs`, `crates/platform/engine/src/operation.rs::next_scope`.

**Facts**

- A component id is a fresh UUID: `uuid::Uuid::now_v7()` in `WorkloadService::new` and `WorkloadComponent::new`. It does not contain the workload id.
- Upstream says so in its own test comment: component ids "share nothing with their workload's id". Its built-in Postgres plugin tags each entry with the workload id and compares exactly.
- wamn registers claims under the component id (`mod.rs::on_workload_item_bind` passes `item.id()`).
- On unbind wamn gets the workload id and keeps every entry where `!id.starts_with(workload_id)` (`claims.rs::clear_component_claims`, `statements.rs::clear_statement_bindings`).
- A UUID does not start with the workload id. So the cleanup removes nothing that was registered under a component id.
- The comment in `claims.rs` says component ids are prefixed by the workload id. At v2.11.0 that is false.
- The test `clear_component_claims_reaps_all_registries_for_the_workload` uses invented ids such as `wl-a-component-0`. It passes for ids the runtime never produces.

**What this means**

- The risk in the first report (one workload clearing another) does not occur.
- The real defect: on the wash path, entries of stopped components are never removed. Each restart of a component leaves its env\_key, project and login-class entries behind until the host exits.
- *Inference:* the effect is slow memory growth only. A dead UUID is never used again, so no later call can read the old entries. Few components use this path (`http-route`, the materializer), and restarts are capped.
- Not affected: per-request scopes. They are named `<component id>#<n>` (`next_scope`) and are removed exactly when the request ends (`operation.rs::OpenOperation::drop`, `native_policy.rs`).
- The one entry the prefix does remove: `operation_workloads[workload_id]`, because that key is the workload id itself.

**Options**

- **Option A.** Record the workload id with each entry at bind (`item.workload_id()` is available there). On unbind, remove entries whose recorded workload id is equal. This is what upstream does. With the one-map ruling (tab 11 finding 2) it is one field on the record. The false comment goes. The test is rewritten with a UUID component id and a separate workload id.
- **Option B.** Delete `clear_component_claims` and `clear_statement_bindings` as code that does nothing, and accept the growth.

**Carried to part 6:** `wamn_jetstream.rs` and `wamn_logging.rs` register per-component entries and have no unbind handler at all. Same growth, to be confirmed when they are read.

**Ruled 2026-11-11, finding 8: option A.** Each entry records its workload id at bind. Unbind removes entries with an equal workload id. Tab 13 is fully ruled.

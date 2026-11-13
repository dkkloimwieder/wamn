# 11 wamn-runtime, part 2: Postgres plugin entry, claims, pools

Nothing was built or run. Every statement is from reading the code unless marked *inference*.

## What was read

| File (under `crates/platform/runtime/src/plugins/wamn_postgres/`) | Lines without tests |
| --- | --- |
| `mod.rs` | 578 |
| `claims.rs` | 2,610 |
| `claims/pools.rs` | 316 |
| `claims/transactions.rs` | 660 |
| `pool.rs` | 754 |
| `credential_exactness.rs` | 576 |

Callers were searched across `crates/`, `services/`, `apps/`, `deploy/` and `tools/`.

## One purpose

Give a component a database connection that carries the right login and the right claims.

## What serves another purpose

`claims.rs` holds seven separate things.

| # | What | Where | Belongs with |
| --- | --- | --- | --- |
| 1 | Plugin state and the claim maps | `claims.rs::WamnPostgres`, `SessionClaims` | stays |
| 2 | Text checks on SQL | `claims.rs::reject_claim_mutation` and helpers | stays, own module |
| 3 | Who the caller is and what it may do | `principal_grants`, `held_operation_grants`, `user_operation_permissions`, `session_operation_permissions`, `queued_run_caller`, `workflow_node_caller`, `environment_inactive` | `plugins/route_authentication.rs` and `auth_cache.rs` |
| 4 | HTTP effect authorization | `ConnectionEffectLookup`, `ConnectionEffectSnapshot`, `CONNECTION_EFFECT_SNAPSHOT_SQL`, `connection_effect_snapshot`, `read_effect_snapshot`, `EffectKey`, `EffectFixed` (about 520 lines) | `plugins/connection_http.rs`, its only caller |
| 5 | The value behind weak ETags | `model_version` | the read cache in `execution/host` |
| 6 | Host route transaction | `administration_transaction` | stays |
| 7 | Constructors | `new`, `from_env`, `from_env_for_project`, `with_provider` | stays |

**Option A.** Move 3 and 4 to their owners. The plugin offers one thing to them: check out a connection for a named login. About 900 lines leave this plugin.

**Option B.** Keep them here, each in its own module. No line count change.

## Rulings that land here

| Ruling | What goes here |
| --- | --- |
| One login per role | `pool.rs::PoolKey.generation_role`, `pool.rs::credential_generation_role`. See finding 1. |
| Candidate branch deleted (part 1) | `CandidateConnectionBinding`, `CandidateBindingWorld`, `ConnectionEffectLookup.candidate_binding`, `ConnectionEffectSnapshot.pinned_generation` (about 95 lines). Users: `connection_http.rs` (4 places), `local_application.rs` (1), `production_claim.rs` (2). The connection plugin part confirms the users. |
| `wamn-pg-core` deleted | `claims/transactions.rs::causation_emit_sql`. Test only. It goes with its test. |
| Flow rows and `run-stream` deleted | The `mod.rs` header still names `run-stream`. |

## Findings, each with an option

### Single truth

**1. Every checkout resolves the credential before it looks for the pool.** `claims/pools.rs::ensure_pool` calls `provider.resolve` first. For an application login that copies the URL, parses it twice and computes a SHA-256 (`pool.rs::StaticCredentialProvider::resolve`). Then `credential_generation_role` parses it a third time. The code says the order exists only because the pool key holds the credential generation.

- Option: after the one-login ruling the key is project, login class and env\_key. Look up first. Resolve only when no pool exists.

**2. One claim set is stored in eleven maps.** `WamnPostgres` has a separate locked map for env\_key, project, schema, runner, role, user, operation, application, release, current run and invocation. `SessionClaims` is already the struct for nine of them. `write_session_claims`, `revoke_session_claims`, `clear_component_claims` and `session_claims` each walk all maps by hand. The comment on `SessionClaims` says a map without a matching field is a leak.

- Option: one map from component id to `SessionClaims`. Nine setter and getter pairs go. Estimate: 400 lines.
- Related fact: `set_role`, `set_user_id`, `set_operation`, `set_application`, `set_release_identity` are public. Outside this plugin only tests call them.

**3. `begin_with_claims` takes twelve arguments in order.** Twenty call sites, seven of them in `production_claim.rs`. Most pass `None` five or six times.

- Option: pass `&SessionClaims`. Goes with finding 2.

**4. The roles and permissions of a principal are read by three SQL statements and folded by three loops.** `claims.rs::PRINCIPAL_GRANTS_SQL`, `claims.rs::USER_OPERATION_PERMISSIONS_SQL`, `route_authentication.rs::QUEUED_RUN_CALLER_SQL`. Folds: `principal_grants_from_rows`, `HeldOperationGrants::from_rows`, the loop in `route_authentication.rs::queued_run_caller`.

- Fact: production calls `queued_run_caller` with a role every time (`execution/workflow/src/queue.rs`). The branch without a role is the service principal branch that part 1 deletes.
- Fact: `workflow_node_caller` already gives the same answer from `principal_grants`.
- Option: one read, `principal_grants`. `held_operation_grants` and the queue use it. Two statements and two folds go.
- *Inference:* the answers are equal. `held_operation_grants` reads under the administration login and the others under the HTTP login. Check grants at change.

**5. The statement timeout is set three times with one value.** At connect (`pool.rs::session_statement_timeout_hook`), in every transaction (`CLAIM_SQL` and `GUEST_CLAIM_SQL`), and before reads outside a transaction (`GUEST_AUTOCOMMIT_SETTINGS_SQL`). The value comes from the pool each time in the files read.

- Option: keep the connect hook. Drop the parameter from the three statements.
- Not checked: the timeout argument at the sixteen call sites outside these files.

**6. Five settings are held in three structs.** `WamnPostgresConfig`, `ProjectConfig`, `ResolvedCredential` each carry both pool sizes, wait timeout, statement timeout and row limit. `from_global_config`, `from_global`, `select` and the JSON reader copy them field by field.

- Option: one settings struct that the other types hold.

**7. One database URL is parsed in five places.** `credential_exactness.rs::explicit_credential_source`, `pool.rs::credential_generation_role`, `pool.rs::credential_database`, inline in `pool.rs::credential_exactness_hook`, `claims/pools.rs::build_pool`.

- Option: parse once when the project configuration is built. Hold the parsed value.

**8. The workload configuration names are defined twice.** `wamn.env_key` and `wamn.project` are constants in `wamn_postgres/mod.rs` and again in `wamn_logging.rs`. `wamn_jetstream.rs` imports the Postgres copy.

- Option: one definition in `plugins/mod.rs`.

**9. The WIT binding block is written twice.** `mod.rs` has one `bindgen!` with the feature `wasm_component_model_implements` and one without. Both production users turn the feature on (`services/host/Cargo.toml`, `crates/execution/host/Cargo.toml`).

- Option: make it always on. One block and twelve `cfg` lines go.
- Check at change: test targets that build the runtime without the feature.

### Dead code

**10. Two claims are set and never read.** `app.runner` is set by all three statements. No SQL in `deploy/`, `crates/` or `services/` reads it. Only `tools/bench/pg-floor/*.sql` does. `app.application` is set by two statements. No SQL reads it. The comment says it is for logging.

- Option: drop both from the statements. `SessionSettings.runner` goes too. The host keeps both values in memory, where the claim code and numbering already read them.
- *Inference:* `production_claim.rs` passes the runner as a bind parameter. Confirmed in that part.

**11. `mod.rs::add_to_linker` has no caller.**

- Option: delete.

**12. `probe_checkout`, `probe_checkout_of`, `CheckoutProbe` are called by tests only.**

- Option: delete. Correction: one test calls it twice, to read a backend id. That test changes; it is not deleted. See `claims/tests.rs`.

**13. Parts of the credential probe can never fire.**

- `AmbientCredentialState`: every caller passes `Absent`. `SourceConflict` cannot happen.
- `CredentialConnectionKind::Listener` and `probe_listener`: no caller. The type then has one value.
- `MembershipMode::Usage` and `Set`: no caller.
- The env\_key binding check compares a value with itself. `pool.rs::credential_exactness_hook` passes `project` to both sides. `services/scenario-worker/src/store/admission.rs::admission_credential_probe` passes `env_key` to both sides. No server answer is involved.
- Option: delete all four. Estimate: 150 lines.

**14. Most server checks in the probe compare the server with the URL just used to connect.** Session user and database come from the URL. The role membership check is the one that asks something the URL cannot answer.

- Option A: keep membership and current user. Drop session user and database.
- Option B: keep all. Cost: none per request, one query per new connection.
- *Inference:* session user and database cannot differ on a direct connection.

**15. `SCHEMA_CONFIG_KEY` and `RUNNER_CONFIG_KEY` have no producer.** No deploy file and no control code writes `wamn.schema` or `wamn.runner`. One test fixture writes `wamn.schema`.

- Option: delete both keys and their branches in `mod.rs::register_workload_scope`.

**16. `ClassCredentials::every_class` does nothing in production.** `services/host/src/host.rs::host_credentials` calls it, then sets or removes each of the other four classes. The result is always the application login alone plus the named ones. `without_class` exists to undo `every_class`. The JSON key `"url"` and `projects_from_json` are used by tests only. The reader also refuses an old key, `pool_max_size`.

- Option: delete `every_class`, `without_class`, the `"url"` key and the old-key refusal. `host_credentials` builds the map from five optional URLs. Tests name the class.

**17. The default project has no production user.** `services/host` builds with `from_env_for_project`. Every production `SessionClaims` names a project. `DEFAULT_PROJECT`, `WamnPostgres::new`, `from_env`, `default_only`, `from_global_config` and the fallback in `project_for` serve tests. Two other users: `wamn_jetstream.rs` falls back to it, and `deploy/platform/materializer.example.yaml` writes `wamn.project: default`.

- Option A: project is required. Delete the five. About thirty test sites name a project.
- Option B: keep. A project that is not named silently becomes `default`.

**18. `CredentialProvider` is a trait with one production implementation.** The second implementation records calls in one test.

- Option: delete the trait. The plugin holds the project map.

### Safety checks on SQL text

**19. `reject_claim_mutation` runs four text block lists.** Session changes, `wamn.` logical messages, counter functions, writes to `platform.`. Its own comment says a `DO` block passes the first list.

- Fact: after the ruling on dynamic SQL, application SQL runs in a read-only transaction. Postgres itself then refuses the platform writes.
- Option A: keep all four for platform components. For applications keep only the session list.
- Option B: keep all four everywhere.
- Not checked: whether `pg_logical_emit_message` and the counter functions are refused in a read-only transaction. `set_config` is not refused there, so the session list stays in both options.

### Comments

**20. Comments carry history, not behavior.** Issue ids, stage names (`S2..S6`), `pgbench`, capital-letter emphasis, and text about states that no longer exist.

- Option: rewrite when the code under them changes.

## Carried from other tabs

- `claims.rs::valid_user_id` is one more UUID check. It moves to `wamn-scalar` (ruled in tab 05).
- `valid_runner`, `valid_schema` arrive here from `wamn-control-registry` (ruled in tab 02). With findings 10 and 15, `valid_runner` may have no caller left. Check in the `production_claim.rs` part.

## Not read

- `claims/tests.rs` (3,957 lines) and the test module of `credential_exactness.rs`.
- `connection_http.rs` and `local_application.rs` beyond the lines that name the candidate binding.
- The sixteen `begin_with_claims` call sites outside these files.

## Rulings (2026-11-11)

- Split: **option A**. Authorization moves to `route_authentication.rs` and `auth_cache.rs`. HTTP effect authorization moves to `connection_http.rs`.
- Findings 1 to 18 and 20: accepted, delete or merge option each time.
- Finding 14: option A. Keep membership and current user.
- Finding 17: option A. Project is required.
- Finding 19: owner asked for the open check first. Result below. Not ruled.
- New rule: tests are marked for deletion together with the code they test.

## Finding 19, checked

Nothing was run. Sources: Postgres source and `deploy/sql/app-schema.sql`.

| Block list | Refused by Postgres in a read-only transaction? | Source |
| --- | --- | --- |
| Writes to `platform.` | Yes | Postgres refuses INSERT, UPDATE, DELETE, MERGE, TRUNCATE, COPY FROM there |
| Counter functions | Yes | `platform.bump_counter`, `assign_counter`, `set_counter` each write `platform.number_counter` |
| `wamn.` logical messages | **No** | `pg_logical_emit_message` and `LogLogicalMessage` have no read-only check (`logicalfuncs.c`, `message.c`) |
| `SET`, `RESET`, `set_config` | **No** | not a write |

**21. New finding: no check refuses transaction control text.** `claims.rs::statement_mutates_session` matches `SET`, `RESET` and `set_config` only. A statement `COMMIT` ends the host transaction. The next statement then runs outside it: read-write, and with no claims.

- Fact: no file read so far refuses `COMMIT`, `ROLLBACK`, `END`, `ABORT`, `BEGIN` or `START TRANSACTION` as statement text.
- *Inference:* this defeats the read-only ruling for application SQL on the transaction resource. Not run.
- Not read yet: `resources.rs` in full. It may refuse this by another path. Part 3 confirms.

**Options for 19 and 21 together**

- **Option A, text.** Application SQL keeps two lists: session changes and `wamn.` messages. The session list gains the six transaction control words. The counter and `platform.` lists stop running for application SQL. They stay for platform components, which write.
- **Option B, grants.** Application dynamic SQL connects as a login that holds SELECT only. Postgres refuses every write without text checks. The `wamn.` message list and `set_config` check still stay. Cost: one more login and one more pool per environment.
- **Option C.** Keep all four lists everywhere and add the six words.

A and C rest on text matching, which the code comment itself calls incomplete. B does not.

## Tests that go or change with the code

**Delete**

| Test | File | Goes with |
| --- | --- | --- |
| `candidate_binding_world_requires_complete_canonical_unique_rows` | `claims/tests.rs` | candidate branch |
| `the_composed_credential_becomes_the_default_project` | `claims/tests.rs` | 17 |
| `causation_emit_sql_pins_the_transactional_wamn_message`, `causation_emit_sql_escapes_single_quotes_in_the_run_id` | `claims/tests.rs` | `wamn-pg-core` |
| `live_stream_reads_once_and_scopes_claims`, `an_unchecked_declared_statement_is_refused_before_a_stream_opens` | `claims/tests.rs` | `run-stream` (part 1) |
| `an_acquisition_binds_its_application_and_the_next_one_does_not_inherit_it`, `live_a_transaction_carries_only_the_application_its_host_bound` | `claims/tests.rs` | 10. Both read `app.application` back from SQL. |
| `queued_caller_retains_typed_connection_unavailable` | `claims/tests.rs` | 4 |
| `one_configured_url_names_every_class_explicitly` | `pool.rs` | 16 |
| `the_default_config_is_registered_under_the_default_project` | `pool.rs` | 17 |
| `the_credential_generation_is_the_login_role_the_url_carries`, `a_rotated_credential_is_a_different_pool_key` | `pool.rs` | one login per role |
| `the_exactness_hook_refuses_an_unbound_project` | `pool.rs` | 13 |
| `from_env_ignores_an_ambient_database_url` | `pool.rs` | 17. `WamnPostgresConfig.credentials` loses its last reader. |
| `exact_source_rejects_an_ambient_conflict_without_disclosure`, `listener_uses_the_same_expected_identity_contract`, `pooled_and_listener_paths_run_the_same_complete_query_contract` | `credential_exactness.rs` | 13 |
| Two calls `queued_run_caller(&admin, SERVICE, None)` and their assertions | `execution/workflow/src/queue/automation_live.rs` | 4 and part 1 |
| The wrong-binding case | `runtime/tests/executor_platform_surface_live.rs` | 13 |

**Change, not delete**

| Test | File | Why |
| --- | --- | --- |
| `clear_component_claims_reaps_all_registries_for_the_workload` | `claims/tests.rs` | 2. One map. |
| `validate_claims_rejects_malformed_identities`, `live_bound_claims_are_injection_inert_and_txn_local`, `live_begin_with_claims_sets_the_guest_set_without_a_env_key_claim`, `live_autocommit_settings_are_reused_only_on_their_connection` | `claims/tests.rs` | 5 and 10. They read `app.runner`, `app.application` or the timeout. |
| `live_a_transaction_binds_the_executing_principal_and_a_read_stays_autocommit` | `claims/tests.rs` | 12. Uses `probe_checkout`. |
| `two_authorities_never_share_a_pool_key`, `the_guest_env_key_check_reads_the_credential_the_guest_would_connect_with`, `project_config_names_each_lifecycle_budget_and_refuses_the_retired_key`, `an_unlisted_project_does_not_borrow_another_projects_credential` | `pool.rs` | 1, 16, 17 |
| `source_user_database_and_env_key_swaps_refuse`, `pooled_identity_refuses_each_wrong_database_fact` | `credential_exactness.rs` | 13, 14 |
| About thirty sites that call `WamnPostgres::new` or `every_class` | 14 test files | 16, 17 |
| Sites that call `set_user_id`, `set_operation`, `set_schema`, `set_runner`, `set_release_identity` | `runtime/tests/common/mod.rs`, `executor_platform_surface_live.rs`, `sqlx_transaction_live.rs` (already deleted by tab 04) | 2 |
| `wamn.schema` entry | `test-support/infrastructure/fixtures/workload/cross-environment-workload.json` | 15 |
| `app.runner` in two SQL files | `tools/bench/pg-floor/update.sql`, `receipt.sql` | 10 |

**Move with the code (option A)**

| Test | From `claims/tests.rs` to |
| --- | --- |
| `effect_snapshot_refuses_a_env_key_that_disagrees_with_the_bound_claim`, `effect_snapshot_refuses_a_component_with_no_bound_env_key`, `effect_snapshot_checks_out_under_the_callable_http_authority` | `connection_http` tests |
| `user_operation_permissions_reuse_only_the_callable_http_authority`, `session_and_pat_callers_act_with_their_single_role`, `live_a_drop_refuses_a_revoked_pat_on_the_next_request`, `live_sessions_refuse_unheld_service_roles_and_services_have_no_active_user_role`, `live_a_bind_refuses_a_principal_with_no_user_row`, `live_a_removed_user_row_is_refused_once_the_auth_memory_clears`, `live_a_drop_refuses_a_removed_user_row_on_the_next_bind` | `route_authentication` tests |

**Depends on 19**

`guard_rejects_guest_counter_function_calls`, `guard_rejects_guest_platform_writes`, `guard_allows_platform_reads_and_application_writes`.

**Limit of this list.** `claims/tests.rs` was read by test name, doc comment and the lines that name removed symbols. The bodies of the tests in the change table were not read in full.

## Ruling on 19 and 21 (2026-11-11)

**Option B.** Application dynamic SQL connects as a login that holds SELECT only.

What follows from it:

- The counter and `platform.` text lists stop running for application SQL. Their three tests go: `guard_rejects_guest_counter_function_calls`, `guard_rejects_guest_platform_writes`, `guard_allows_platform_reads_and_application_writes`. Open: whether platform components still need the two lists. Decided when their callers are read in part 3.
- The `set_config` check and the `wamn.` message check stay. Reason for the first: a read outside a transaction does not set `app.role` (`claims/transactions.rs::one_shot_statement`), so a session value left on a pooled connection would be read by the next caller.
- Finding 21 loses its write risk. A `COMMIT` in text now only drops the claims, and the login still cannot write.
- The read-only transaction from part 1 stays for one snapshot per request. It is no longer the safety measure.

**How the host picks the login. Open, two options.**

- Option 1: by interface. A call on `wamn:postgres/client` from an application uses the SELECT-only login. A call on `wamn:postgres/statements` uses the application login. No configuration and no new component.
- Option 2: by component. Dynamic SQL lives in its own component, and the host binds the SELECT-only login to that component, the way `mod.rs::AUTHORITY_CONFIG_KEY` binds the materializer today. The application calls that component.

Not read yet: the provisioning code that creates logins and grants. The SELECT-only login is a new role there.

**Ruled 2026-11-11: option 1.** The host picks the login by interface. `wamn:postgres/client` from an application uses the SELECT-only login. `wamn:postgres/statements` uses the application login.

The open point about platform components is answered in tab 12, finding 2: no platform component calls raw SQL.

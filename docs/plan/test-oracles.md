# Tests own no oracle of their own

Sep 23, 2026. Epic 14.

## 1. Rule

A test survives only if a realistic defect fails it, and nothing else rejects that defect more strongly.
The expected value of the test (its oracle) must be independent of the code under test.

- No test compares source text, DDL text, or a generated file to a string.
- A byte or digest test survives only when the bytes are a persisted or wire contract and the expected value is independent.
- A source or repository policy check is a lint in `tools/repo-lint`, not a test.
- No new live test duplicates an existing one. A live test grows only where a deleted test guarded a rule that nothing else covers.

Issue 1 adds this rule to [running tests](../operations/running-tests.md).

## 2. Scope

The targets are in crates that Epic 12 (`wamn-3lw7`) does not move, with one exception.
Target 8 edits `crates/platform/runtime/tests`, and Epic 12 is splitting that crate, so target 8 waits for Epic 12.
The run-state tests (`run_state_boundary.rs`, `store.rs`, `durability.rs`) belong to the Epic 12 agent.

The live tests get a private PostgreSQL 18 server from `wamn_test_postgres::database()`.
They read no environment variable and do not skip themselves, so each rewritten assertion runs in the ordinary workspace sweep.

Each issue names its tests and counts them in four buckets: deleted, rewritten as behavior, moved to lint, and kept.
Each issue runs the workspace build, clippy, fmt, and the tests of the files it touches.
Issue 10 runs the full sweep once.

## 3. Targets

### 3.1 `crates/identity/platform/tests/schema.rs`

The file goes. Deleted 1, rewritten 2.

- `system_schema_has_no_plaintext_identity_credential_column`: deleted. The live digest checks in `pat_live.rs`, `password_live.rs` and `password_login_live.rs` hold the real rule.
- `system_schema_contains_the_platform_identity_core`: rewritten in `identity_live.rs`. The `identity` schema is owned by `wamn_system`. A second `create_service` with the same subject returns a conflict. A project delete removes its `project_roles` and `project_env_memberships` rows.
- `system_schema_stores_personal_access_tokens_as_expirable_digests`: rewritten in `pat_live.rs`. A duplicate token prefix, a malformed prefix or hash, and `expires_at = created_at` each fail with the named SQLSTATE. A principal delete with a live token fails with `foreign_key_violation`.

### 3.2 `crates/identity/project-state/tests/schema.rs`

Rewritten 4, kept 6. All four rewrites go into `app_schema_applies_and_enforces_isolation_on_postgres`.

- `app_schema_sql_mirrors_the_model`: every model table has row security enabled and forced (`pg_class`). Its columns include the model columns (`pg_attribute`). The model is the oracle.
- `tenant_floor_derives_from_the_connected_role`: every table and history table has an index on `wamn_authority.tenant_key(tenant_id)` (`pg_index`). An empty tenant fails with `check_violation` on each base table.
- `user_status_literals_are_pinned`: one user for each `UserStatus::ALL` value inserts.
- `fk_cascades_are_pinned`: a role delete removes its `permissions` and `user_roles` rows.

### 3.3 `crates/control/provision/tests/control_storage.rs`

Deleted 7, rewritten 2, kept 1. The live test `system_schema_applies_and_enforces_invariants_on_postgres` stays and takes the rewrites.

- Deleted: `system_schema_sql_mirrors_the_model`, `retired_configurable_publish_policy_stays_deleted`, `upsert_org_sql_matches_the_placement_columns`, `event_readers_table_and_builders_match_the_columns`, `placement_check_is_present_and_tier_checks_are_gone`, `charset_length_checks_backstop_the_stored_slug_names`, `no_data_plane_manifest_references_the_system_cluster`.
- `upsert_project_and_project_env_sql_match_the_columns`: rewritten. The live test runs `select_org_placement_sql`, `select_env_policies_sql`, `select_retired_project_envs_sql` and `select_org_project_envs_sql`. It asserts the rows, the order, and that no row of another org returns.
- `saga_sql_builders_match_the_core_saga_contract`: rewritten. The live test runs the `saga` builders: create twice is one row, a step moves forward, and select reads the result.
- Uncovered rules that get a live probe: core `sagas` refuses kind `copy`. The name boundaries that the live test does not reach: env name and project id over 40, `pool_cluster` over 63, an id of exactly `wamn`.
- Rules left without a check, named in the close reason: the system database has no row security, retired `orgs` columns stay absent, retired source names stay absent, and `SCHEMA_VERSION` appears in the DDL.

### 3.4 `crates/control/provision/tests/ops_storage.rs`

Deleted 3, rewritten 1, kept 1.

- `packaged_ops_schema_is_the_deploy_artifact`: deleted. `OPS_SCHEMA_SQL` is an `include_str!` of the same file. The live test applies `OPS_SCHEMA_SQL`, and its file-reading helper goes.
- `core_schema_has_no_operations_relations_or_literals`: deleted. The table set in the control storage live test covers the relations. The `copy` literal gets the probe in 3.3.
- `ops_schema_is_additive_idempotent_and_one_way`: deleted. The live test gets two catalog checks: the core table set is the same before and after the ops apply, and the only foreign key out of the ops tables targets `registry.project_envs`.
- `copy_and_dump_builders_match_the_ops_relations`: rewritten. The live test runs `complete_saga_sql`, `fail_saga_sql`, `select_saga_sql` and `select_dumps_sql` from `state.rs`, and a second create that changes nothing.
- `ops_schema_applies_idempotently_after_core_on_postgres`: kept.

### 3.5 `services/scenario-worker/src/store/admission.rs`

Rewritten 1. `publication_append_uses_only_the_server_derived_exact_identity` compares `INSERT_WIRING_SQL` and `EXACT_WIRING_SQL` to strings.
It becomes two cases in `management_surface_authenticates_and_attributes_authoring_commands` (`services/scenario-worker/tests/management_live.rs`).

- An identical `catalog.wirings` row exists, and a publish with a new command id succeeds with one row.
- The same identity exists with another `wiring_hash`, and the publish returns `publish-executable-drift` with the row unchanged.

No live test reaches the append-time drift path today.

### 3.6 `crates/catalog/model/src/wiring_activation.rs`

Deleted 1. `the_only_pointer_write_carries_both_the_hash_and_the_enabled_flag` goes.
`crates/catalog/model/tests/wiring_activation_live.rs` runs `flip_activation` through insert, update, rollback, an aborted flip, and the dark state.

### 3.7 `tests/integration/src/membership_test.rs`

Deleted 1. This is an integration code change, not only a test change.
`OPERATION_GRANT` is read from the `grant` field of `apps/wamn_receiving/generated/contracts/purchase_order/get.operation.json`.
`permission_uses_the_generated_canonical_operation_grant` goes.

### 3.8 `crates/platform/runtime/tests/production_claim_live.rs`

Deleted 1. This target waits for Epic 12.

- `production_claim_run_state_stand_in_tracks_schema_of_record`: deleted.
- `tests/common/mod.rs` applies `deploy/sql/run-state.sql` and `deploy/sql/run-queue.sql` in place of `run_state_stand_in_ddl()`. The schema becomes `wamn_run`.
- The fixture adds what the canonical schema needs: `wamn_catalog::test_database::tenant()` for the roles and `catalog.effective_releases`, seed releases 1 and 2, and the executor platform group membership.
- `production_claim_durable_live.rs` shares the fixture and changes with it.
- The `test-util` feature on the `wamn-run-state` dev dependency goes if nothing else uses it. Nothing in `crates/execution/run-state` changes.

### 3.9 `tests/conformance`

Deleted 2, rewritten 1, moved to lint 8.

The lints stay in Rust as one binary, `repo-policy`, in the conformance package, and `tools/repo-lint` runs it as a leg.
`tests/conformance/tests/repo_lint.rs` names the new leg.

- `docker_component_provenance.rs`: its 4 tests move to lint. The synthetic scanner checks inside them go.
- `version_identity.rs`: 3 tests move to lint. `representative_version_mutants_are_rejected` and `a_missing_watched_file_still_reports_its_missing_occurrence` are deleted. The `INVOCATION_CONTEXT_VERSION` check reads the constant in the lint binary.
- `tests/session_claims.rs`: its test moves to lint and stays. Its synthetic scanner checks go.
- `invocation.rs`: `node_abi_is_live_versioned_and_router_shaped` parses `crates/execution/router/wit/package.wit` with `wit-parser` (`=0.252.0`, the version `wamn-runtime` uses) and asserts on the resolved package, world, and types. The generator does not parse WIT, so the workspace parser is the tool.

### 3.10 Documentation and closeout

The closeout lists the per-issue counts, runs the full sweep once, and compares it with the sweep of `wamn-7icx`.

## 4. Not in scope

Found while scoping, with the same kind of test, and filed as one finding:

- `crates/control/registry/src/sql.rs` tests
- `crates/control/lib/src/author_wiring.rs` and `promote.rs` SQL text checks
- `no_statement_lets_the_caller_choose_the_tenant` in `wiring_activation.rs`
- the six scanner tests in `tests/conformance/tests/repo_lint.rs`
- `the_publish_surface_is_column_exact_at_the_runtime_boundary` in `admission.rs`

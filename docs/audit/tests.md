# Audit lane G: Tests

Three read-only Codex gpt-6.1-sol runs at reasoning effort high wrote this file, one per part, on 2026-10-31 against commit 8c831349d. Root copied each run's final message verbatim and checked that every finding cites a file and line. Beads: wamn-lybom.1, wamn-lybom.2, wamn-lybom.3. Shared tool output: [measurements.md](measurements.md).

<!-- lane G1, bead wamn-lybom.1 -->

## Part 1: duplication

This is a static audit. No files changed, and no builds or tests ran. Locations below identify independent definitions, not repeated imports of one definition.

## Finding G1-1: Shared support needs one owner and a smaller dependency boundary

Severity: medium  
Effort: L  
Area: test-support/*  
Evidence: `test-support/harness/Cargo.toml:14`, `test-support/harness/Cargo.toml:28`, `test-support/infrastructure/Cargo.toml:15`, `test-support/infrastructure/lib.rs:13`, `test-support/fixture-package/src/lib.rs:9`, `crates/control/lib/Cargo.toml:98`.

Problem: Harness and infrastructure overlap in command execution, private files, cluster access, and environment preparation. Fixture-package owns separate package paths and contents. PostgreSQL owns the server lifecycle and also serves production upgrade qualification, so merging all four crates creates a dependency problem.

Proposal: Make infrastructure the shared test library and absorb harness and fixture-package. Give it a lightweight default for files, paths, commands, PostgreSQL connections, clocks, and fixture data. Keep deployment helpers optional, keep assertions with their suites, and retain the PostgreSQL lifecycle crate as a separate production dependency. Type-specific builders belong behind the owning crate’s existing `test-util` boundary.

Deletes: Two support manifests, duplicate primitive implementations listed below, and cross-suite source imports such as `tests/integration/src/control_client_live.rs:28`. These imports already share source and are not additional helper definitions.

Risk: A merged library can force runtime dependencies into small tests. Dependency inspection must preserve the engine boundary, while existing `guest_workspace_closure` tests retain guest isolation. Measurements.md section 2 reports zero source lines for infrastructure because its sources sit outside `src/`; that row does not establish that the crate is empty.

## Finding G1-2: PostgreSQL connection setup repeats across fifty helpers

Severity: medium  
Effort: M  
Area: crates/*/tests, services/*/tests, tests/integration, inline tests  
Evidence: The following helpers repeat the connect-and-spawn kernel.

Under `crates/control/lib/tests/`: `environment_bindings_live.rs:8`, `capture_gap_live.rs:34`, `connection_binding_live.rs:65`, `workflow_schedule_live.rs:170`, `replica_identity_live.rs:8`, `platform_grants_live.rs:24`, `workload_generation_live.rs:51`, `app_generation_isolation_live.rs:60`, `package_data_access_live.rs:22`, `run_plane_live.rs:369`, `apply_package_live.rs:20`, `workflow_lifecycle_live.rs:60`, `connector_restart_live.rs:54`, `control_generation_live.rs:30`, `workflow_manual_run_live.rs:254`, `create_user_live.rs:24`, `provision_org_admin_live.rs:17`, `publish_release_live.rs:21`, `integration_principal_live.rs:15`, and `support/files.rs:25`.

Under `crates/platform/runtime/`: `tests/session_route_authentication.rs:53`, `tests/sqlx_transaction_live.rs:68`, `tests/effect_snapshot_live.rs:22`, `tests/executor_platform_surface_live.rs:68`, `tests/environment_status_route.rs:35`, `tests/common/mod.rs:36`, `src/plugins/wamn_postgres/numbering/tests/live.rs:60`, and `src/plugins/route_authentication/tests/connection_live.rs:11`.

Other crate definitions: `crates/control/lib/src/qualify_upgrade/scratch.rs:298`, `crates/schema/introspection/tests/postgres_live.rs:206`, `crates/execution/host/tests/support/application_fixture.rs:37`, `crates/execution/host/tests/support/control_fixture.rs:19`, `crates/execution/host/src/operation/native_policy/tests/authenticated.rs:57`, `crates/identity/platform/tests/password_login_live.rs:14`, `crates/identity/platform/tests/session_keys_live.rs:526`, `crates/identity/platform/tests/password_live.rs:27`, and `crates/identity/platform/tests/auth_drops_live.rs:42`.

Application and integration definitions: `apps/wamn_receiving/tests/receiving_history/database.rs:53`, `apps/wamn_receiving/tests/receiving_data_access.rs:1345`, `tests/integration/src/cdcbench.rs:164`, and `tests/integration/src/membership_test.rs:189`.

Service definitions: `services/ctl/tests/identity_issuer_live.rs:65`, `services/ctl/tests/session_audience_live.rs:69`, `services/ctl/tests/prune_run_history_live.rs:45`, `services/ctl/tests/protected_relations_live.rs:35`, `services/ctl/tests/prune_record_history_live.rs:56`, `services/scenario-worker/tests/management_live.rs:289`, `services/identity/tests/https_surface.rs:372`, `services/identity/tests/session_exchange.rs:1469`, and `services/cdc-reader/tests/event_reader_live.rs:92`.

Problem: The same transport setup repeatedly chooses how to drive and report connection failures. Some versions discard errors, some panic, and others retain the driver handle. Several helpers also set roles, actor identity, or timeouts, so replacing their entire bodies would change test behavior.

Proposal: Share the connection-and-driver primitive. Keep role selection, actor identity, deliberate disconnections, and test-specific timeouts explicit at each caller.

Deletes: Repeated `tokio_postgres::connect` and driver-spawn bodies. Retain the distinct session preparation.

Risk: A shared helper must permit deliberate connection termination. The rollback cases in `session_keys_live` and the existing `connection_binding_live` target exercise this distinction.

## Finding G1-3: PostgreSQL command runners already disagree on execution rules

Severity: medium  
Effort: M  
Area: crates/control/provision/tests, crates/execution/run-state/tests  
Evidence: `psql` or `psql_output` appears at `crates/control/provision/tests/database_owner.rs:34`, `provision.rs:89`, `cdc.rs:39`, `operation_grants.rs:21`, `identity_issuer_live.rs:26`, `session_role_reader_live.rs:26`, `system_reader_grants.rs:44`, `family_surface_grants.rs:56`, `family_denial_matrix.rs:423`, `tenant_key_live.rs:84`, `control_portable_store.rs:10`, and `deploy_sql_authority.rs:188`. Further definitions sit at `crates/execution/run-state/tests/run_state_live.rs:19` and `crates/execution/run-state/tests/admission_live.rs:15`.

Problem: Three runners have identical bodies, while the wider family repeats process setup with different flags and output conversions. One version removes the password from arguments and bounds execution. Others pass the complete URL, and transaction handling also differs.

Proposal: Share one runner that returns status and unmodified output. Keep transaction selection, actor preambles, and success or refusal assertions at the caller. Preserve verbose SQLSTATE output for permission tests.

Deletes: Fourteen local runners and their repeated pipe setup. Small success and refusal wrappers can disappear where direct assertions remain clear.

Risk: Applying one transaction policy everywhere breaks scripts that create databases or use `\connect`. Retain `project_env_database_ownership_and_connect_are_scoped`, the CDC tests, and the existing `admission_live` refusal cases.

## Finding G1-4: Database URL rewriting has several independent implementations

Severity: medium  
Effort: S  
Area: PostgreSQL test helpers  
Evidence: `swap_db` repeats at `tests/integration/src/cdcbench.rs:151`, `services/cdc-reader/tests/event_reader_live.rs:62`, and `crates/control/provision/tests/cdc.rs:22`.

Credential replacement repeats as `role_url` at `crates/control/provision/tests/system_reader_grants.rs:82`, `family_surface_grants.rs:147`, `tenant_key_live.rs:378`, `tests/integration/src/cdcbench.rs:157`, and `services/cdc-reader/tests/event_reader_live.rs:73`. Equivalent `login_url` helpers sit at `crates/control/provision/tests/identity_issuer_live.rs:74`, `session_role_reader_live.rs:109`, `family_denial_matrix.rs:509`, and `services/identity/tests/session_exchange.rs:1484`.

The `generation_url` variants sit at `crates/control/lib/tests/package_data_access_live.rs:38`, `crates/platform/runtime/tests/executor_platform_surface_live.rs:80`, `tests/integration/src/trusted_http_route.rs:781`, and `tests/integration/src/local_application/assembly.rs:522`. An existing credential operation is available at `test-support/postgres/lib.rs:196`.

Problem: Some helpers parse URLs, while others split strings around `/`, `@`, or the scheme. They encode different assumptions about credentials and query parameters. The duplicated operation is independent of the tested role family.

Proposal: Share parsed credential and database replacement operations. Pass the password and database explicitly rather than embedding package constants in the helper.

Deletes: Three `swap_db` bodies and the repeated parsing or string-splitting bodies in the credential helpers.

Risk: Replacement must preserve the caller’s required connection parameters. Retain the session-role reader test, which already handles literal `+` and `&` password bytes.

## Finding G1-5: Cluster command helpers repeat cancellation and diagnostic policy

Severity: medium  
Effort: M  
Area: test-support, application cluster tests, tests/integration/src/rc  
Evidence: `checked` repeats at `test-support/infrastructure/workload.rs:875`, `release_chart.rs:602`, `platform.rs:220`, `delivery_cluster.rs:603`, `test-support/harness/src/session_issuer.rs:738`, `apps/wamn_wms/tests/cluster/deployment.rs:313`, `apps/wamn_receiving/tests/route_authentication_live/cluster/resources.rs:270`, `apps/wamn_receiving/tests/examples/delivery_timings.rs:181`, and `tests/integration/src/rc.rs:264`.

The associated `kubectl` constructors sit at `test-support/infrastructure/workload.rs:862`, `release_chart.rs:590`, `platform.rs:210`, `delivery_cluster.rs:73`, `test-support/harness/src/session_issuer.rs:723`, `apps/wamn_wms/tests/cluster/deployment.rs:303`, `apps/wamn_receiving/tests/route_authentication_live/cluster.rs:658`, and `tests/integration/src/rc.rs:252`. `command_json` repeats at `workload.rs:871`, `release_chart.rs:599`, and `tests/integration/src/rc.rs:283`.

Problem: Three command helpers differ only in diagnostic text. Other copies vary cancellation, standard input, stderr retention, and secret suppression. These differences now live in each suite rather than in one reviewed execution boundary.

Proposal: Share command capture and explicit kubeconfig/context construction. Keep public diagnostics and private output handling distinct. Let each suite name its operation and required timeout.

Deletes: Nine command-capture bodies, eight cluster command constructors, and three JSON capture wrappers.

Risk: Combining these helpers can expose private stderr or lose cancellation. Retain `failed_image_build_retains_command_status_and_output_bytes` and the fake-command cases in `kubernetes_gate_runner`.

## Finding G1-6: Private file creation repeats nine times with different overwrite rules

Severity: medium  
Effort: S  
Area: test-support and cluster fixtures  
Evidence: `write_private` appears at `test-support/infrastructure/workload.rs:62`, `event_broker.rs:251`, `delivery_cluster.rs:589`, `platform.rs:236`, `test-support/infrastructure/examples/event_broker_files.rs:84`, `test-support/harness/src/session_issuer.rs:752`, `apps/wamn_wms/tests/cluster/bootstrap.rs:97`, `apps/wamn_receiving/tests/route_authentication_live/cluster/resources.rs:260`, and `tests/integration/src/rc/resources.rs:290`.

Problem: Three implementations are identical. The wider family chooses either exclusive creation or truncation under the same helper name. Moving callers between these implementations can therefore change fixture ownership.

Proposal: Share two clearly named operations: create a private file and replace an owned private file. Preserve the distinction at callers.

Deletes: Nine local implementations and their repeated permission imports.

Risk: Exclusive creation must continue to refuse existing files. Retain `private_files_keep_passwords_out_of_debug_and_bind_the_inspected_server`; make sure that the replacement path also preserves mode `0600`.

## Finding G1-7: Three package manifest fixtures differ only in names

Severity: medium  
Effort: M  
Area: package fixtures and local environment inputs  
Evidence: `crates/control/lib/tests/fixtures/component_package/wamn.json:1`, `crates/control/lib/tests/fixtures/connection_binding/wamn.json:1`, and `tests/integration/fixtures/route_auth_package/wamn.json:1`.

The corresponding environment shapes repeat at `tests/integration/fixtures/local-environments/platform.k:3`, `receiving.k:3`, and `wms.k:3`. `tests/integration/fixtures/local-environments/acme-receiving.k:11` adds a real overlay difference.

Problem: The three JSON manifests repeat the same model, operation, audit declaration, and PostgreSQL component shape. Their differences are package, policy, schema, table, and component names. The environment inputs similarly repeat one document shape, although the Acme package list represents distinct behavior.

Proposal: Use one minimal package fixture with explicit identity inputs. Derive ordinary environment inputs from one fixture function and retain the overlay case. Keep migration differences explicit because those SQL files are not interchangeable.

Deletes: Two complete manifest copies and repeated environment boilerplate. Preserve assertions for component loading, connection binding, route authentication, and overlay composition.

Risk: Reusing the real platform fixture wholesale can introduce unrelated models or effects. Retain the existing `connection_binding_live`, `application_composition`, and route authentication tests against the smaller fixture.

## Finding G1-8: Temporary directories, executable files, and repository paths repeat

Severity: low  
Effort: M  
Area: tests/conformance, tests/system, crate test support  
Evidence: `TestDirectory` construction, path access, and cleanup repeat at `tests/conformance/tests/cranelift_dev.rs:14`, `contract_diff.rs:66`, `repo_lint.rs:70`, `journey_image_cache.rs:44`, `kind_gate_image_remove.rs:113`, and `kubernetes_gate_runner.rs:119`. Shared directory ownership already exists at `test-support/infrastructure/scratch.rs:9`.

Executable writers repeat at `tests/conformance/tests/cranelift_dev.rs:47`, `wasmcloud_release_check.rs:30`, `contract_diff.rs:99`, `repo_lint.rs:101`, `journey_image_cache.rs:76`, `test_changes.rs:376`, `kind_gate_image_remove.rs:146`, and `kubernetes_gate_runner.rs:152`.

Repository-root helpers repeat at `tests/conformance/tests/registry_image_archive.rs:25`, `cranelift_dev.rs:39`, `wasmcloud_release_check.rs:11`, `capability_registry.rs:16`, `contract_diff.rs:91`, `guest_workspace_closure.rs:47`, `repo_lint.rs:93`, `otel_collector.rs:17`, `journey_image_cache.rs:68`, `test_changes.rs:368`, `kind_gate_image_remove.rs:138`, `effect_spans.rs:260`, `palette_declarations.rs:7`, `gate_jobs.rs:9`, `chart_seam_governance.rs:21`, `kubernetes_gate_runner.rs:144`, and `wasmtime_source_identity.rs:49`.

Further root helpers sit at `tests/conformance/src/manifest_dependencies.rs:269`, `ip_name_lookup.rs:40`, `runtime_policy.rs:58`, `tests/system/tests/deployment_declarations.rs:151`, `test-support/fixture-package/src/lib.rs:28`, `test-support/infrastructure/examples/host_values_files.rs:231`, `crates/control/lib/tests/release_chart_render.rs:23`, `package_digest.rs:11`, `edge_bundle.rs:11`, `release_one_digest.rs:41`, `crates/schema/generator/tests/support/client_component_check.rs:78`, `client_ts_check.rs:157`, `apps/wamn_wms/tests/environment.rs:96`, `apps/wamn_receiving/tests/route_authentication_live/environment.rs:119`, and `apps/rest_connector/tests/support.rs:14`.

Problem: Six directory types repeat ownership and cleanup, while eight functions repeat executable creation. Repository location appears under several names and traversal styles. These helpers maintain filesystem mechanics rather than test behavior.

Proposal: Share directory ownership, executable creation, and one repository-path function. Retain caller-selected names, including paths with spaces that test shell quoting.

Deletes: Local directory structs and their methods, executable writers, and repeated repository traversal.

Risk: Normalizing paths can remove deliberate quoting cases. Retain `repo_lint_uses_cargo_owned_workspace_selection_from_any_directory` and the existing contract-diff invocation assertions.

## Finding G1-9: An admitted-component fixture repeats thirty-two lines three times

Severity: medium  
Effort: S  
Area: engine and runtime artifact tests  
Evidence: `crates/platform/engine/src/artifact_source.rs:276`, `crates/platform/runtime/src/component_artifact_source.rs:457`, and `crates/platform/runtime/tests/metadata_token.rs:161`.

Problem: All three `admitted` helpers construct the same package scope, operation ports, component digest, and logging import. A component declaration change requires the same fixture edit in three places. Each test then varies the artifact source rather than this declaration.

Proposal: Put the minimal admitted-component builder behind catalog’s existing `test-util` boundary. Supply component bytes explicitly and leave source behavior in the tests.

Deletes: Two thirty-two-line helper copies and imports used only by them.

Risk: Artifact-source expectations must retain exact digest and declaration identity. Retain `local_source_accepts_exact_bytes_and_refuses_changed_missing_or_invalid_artifacts` and the existing component-pull cases in `metadata_token`.

## Finding G1-10: Router tests maintain three copies of graph construction

Severity: low  
Effort: S  
Area: crates/execution/workflow/router/tests  
Evidence: `node`, `edge_on`, `edge`, and `wiring` appear in `walk.rs:31`, `route.rs:21`, and `terminal.rs:19`. Their edge and compilation definitions sit at `walk.rs:48`, `walk.rs:76`, `route.rs:32`, `route.rs:46`, `terminal.rs:30`, and `terminal.rs:44`.

Problem: Three test targets maintain the same graph construction vocabulary. Route and terminal fixtures match closely, while walk adds configurable components, ports, and ordinals. The drivers test different behavior and are not interchangeable.

Proposal: Share the small graph constructors in router-local test support. Keep walk simulation, route invocation, and terminal assertions separate.

Deletes: Duplicate constructors in route and terminal tests. Keep the distinct drivers.

Risk: A shared default must not erase terminal or edge-order differences. Retain `fan_out_order_follows_the_explicit_edge_ordinal` and `a_respond_node_without_a_caller_fails_the_delivery_not_the_router`.

## Finding G1-11: Role observation and cleanup SQL repeat across test owners

Severity: low  
Effort: S  
Area: control and CLI live tests  
Evidence: `state_sql` repeats at `services/ctl/tests/identity_issuer_live.rs:30`, `services/ctl/tests/session_audience_live.rs:28`, and `crates/control/lib/tests/workload_generation_live.rs:261`.

`drop_generation_role` repeats at `crates/control/lib/tests/run_plane_live.rs:605`, `services/ctl/tests/prune_run_history_live.rs:77`, and `services/ctl/tests/prune_record_history_live.rs:213`.

Problem: Three helpers repeat the same generation-state query with the password-presence observation. Three cleanup helpers repeat guarded `DROP OWNED BY` followed by `DROP ROLE`, with different quoting. Their callers test different contracts but need the same supporting observations.

Proposal: Share the observation and disposable-role cleanup operations. Keep expected grants, families, and retained rows in each test.

Deletes: Two state-query copies and two cleanup bodies.

Risk: Cleanup operates only in the named disposable database and requires the process lock. Retain the existing workload-generation, run-history pruning, and record-history pruning targets.

## Finding G1-12: Recording credential stores repeat as three local types

Severity: low  
Effort: S  
Area: crates/identity/platform  
Evidence: `crates/identity/platform/tests/integration_administration.rs:10`, `crates/identity/platform/tests/connection_drops_live.rs:18`, and `crates/identity/platform/src/connections/administration.rs:584`.

Problem: Each `RecordingStore` records byte values and implements the same `put`, empty `get`, and successful `destroy` behavior. Two implementations match, while the third changes the reference prefix. Maintaining three types adds no independent coverage.

Proposal: Share one recording store behind the credential-store or identity test-support boundary. Make recorded writes available to assertions.

Deletes: Two structs and their three-method trait implementations.

Risk: Tests must continue to inspect the bytes sent to storage. Retain `write_and_copy_keep_environment_generations_and_credentials_separate` and the existing connection-drop cases.

## Finding G1-13: Offline plugin tests repeat the same PostgreSQL configuration

Severity: low  
Effort: S  
Area: crates/platform/runtime/src/plugins  
Evidence: `connection_http.rs:1484`, `wamn_postgres/resources.rs:1841`, and `wamn_blobstore/plugin.rs:1084`.

Problem: Three factories repeat an offline PostgreSQL configuration with no credentials and all limits set to one. HTTP and blobstore also repeat empty connection facts. The plugin types differ, but their supporting state is the same.

Proposal: Share offline PostgreSQL and empty connection-fact builders within runtime test support. Keep the typed plugin constructors local.

Deletes: Repeated configuration literals and empty supporting-state construction.

Risk: Span tests must continue to require no database or object store. Retain `an_effect_span_names_its_originating_wiring_position_and_executing_package` and `a_blobstore_effect_span_names_the_invocation_it_was_raised_under`.

## Finding G1-14: Recursive package copying has three implementations

Severity: low  
Effort: S  
Area: upgrade and Receiving fixture preparation  
Evidence: `crates/control/lib/src/qualify_upgrade/tests/overlay.rs:16`, `apps/wamn_receiving/tests/route_authentication_live/package_copies.rs:124`, and `apps/wamn_receiving/tests/examples/exclusion_diagnostics.rs:37`.

Problem: Three `copy_tree` helpers implement recursive copying. They differ in destination creation and failure handling. Those differences require review whenever a test changes how it prepares an editable package.

Proposal: Share a fallible directory-copy operation with an explicit destination rule. Let callers add their operation context or unwrap inside tests.

Deletes: Two recursive implementations.

Risk: Package copies must retain authored files and generated output required by the caller. Retain `an_edited_copy_builds_in_the_test_owned_workspace` and the existing overlay upgrade tests.

## Finding G1-15: Small clock and ready-future helpers also repeat three times

Severity: low  
Effort: S  
Area: native and guest test helpers  
Evidence: Ready-only future polling appears at `crates/control/provision/src/environment_state.rs:290`, `apps/platform/execution/materializer/src/main.rs:813`, and `apps/platform/data/postgres-statements/src/lib.rs:735`.

Unix-second conversion repeats at `tests/integration/src/host_session_test.rs:231`, `tests/integration/src/identity_session_test.rs:154`, and `services/identity/tests/session_exchange.rs:1508`.

Problem: Three polling helpers implement the same single-poll assertion under different names. Three clock helpers perform the same conversion with different failure messages. These are small copies, so they justify reuse only within the lightweight support library proposed above.

Proposal: Share ready-only polling and fallible Unix-second conversion without adding an executor or clock framework. Keep caller context and credential-safe diagnostics.

Deletes: Two polling bodies and two clock-conversion bodies.

Risk: Ready-only polling must still fail on `Pending`. Retain `callback_error_does_not_commit_or_retry_serialization_failure` and the existing environment-state and materializer unit cases.

## Open questions for the owner

1. Do external consumers require the `wamn-gate-harness` or `wamn-fixture-package` crate names? Internal callers can migrate together.

## Proposed fix epics

### Consolidate shared test support

- Merge harness and fixture-package into infrastructure while preserving the PostgreSQL production dependency
- Share command capture, private files, temporary directories, paths, clocks, and ready-only polling
- Replace cross-suite source imports with explicit fixture APIs

### Consolidate PostgreSQL test mechanics

- Share connections and parsed credential or database replacement
- Share PostgreSQL command capture while preserving transaction and refusal behavior
- Share generation-state observations and disposable-role cleanup

### Consolidate fixture data and test doubles

- Replace name-only package and environment copies with minimal shared fixtures
- Share admitted-component, router graph, recording-store, and offline runtime builders
- Share recursive package copying while retaining upgrade and saved-edit assertions

<!-- lane G2, bead wamn-lybom.2 -->

## Part 2: ignored and pinned

This source audit changed no files and ran no builds or tests. The scan found 136 first-party ignore attributes: 128 prerequisite reasons, seven bead quarantines, and one subprocess helper. Another 127 vendor attributes require external PostgreSQL configurations. The per-crate measurements count text matches, so they also include comments and a generated test fixture (measurements.md section 2).

Most infrastructure ignores are justified: built guests, Helm, Docker/kind, browsers, registries, brokers, external credentials, and controlled performance measurements. The SQLite child helper is deliberately selected by its parent test (`crates/execution/run-state-sqlite/tests/intent_store.rs:160`). The bare ignore in `tests/conformance/tests/contract_diff.rs:275` belongs to generated test input.

## Finding G2-1: Quarantine removes history reconstruction and retention proofs

Severity: medium  
Effort: M  
Area: `crates/control/provision/tests`, `services/ctl/tests`  
Evidence: `crates/control/provision/tests/deploy_sql_authority.rs:2587`; `services/ctl/tests/prune_record_history_live.rs:431`; `services/ctl/tests/prune_record_history_live.rs:631`.  
Problem: `wamn-8vsfz` suppresses history reconstruction, and `wamn-44dr0` suppresses retention behavior and authority checks. Both tests use owned PostgreSQL fixtures rather than external infrastructure. Commit `8b965ac97` records independent failures on main, but this audit does not establish their underlying causes.  
Proposal: Restore default tests against the current history owner. Keep assertions about reconstructed rows, retained positions, and refused identities.  
Deletes: The two quarantine attributes, obsolete fixture setup, and exact CLI prose assertions at `services/ctl/tests/prune_record_history_live.rs:621`.  
Risk: Incorrect pruning can remove retained history or preserve expired entries. `prune_removes_only_the_expired_prefix_of_each_row` and `the_history_read_reconstructs_a_row_at_retained_positions_on_postgres` must pass without ignores.

## Finding G2-2: Fixture grant inventories suppress the private-operation test

Severity: medium  
Effort: M  
Area: `crates/control/provision/src/operation_grants.rs`  
Evidence: `crates/control/provision/src/operation_grants.rs:169`; `crates/control/provision/src/operation_grants.rs:201`; `crates/control/provision/src/operation_grants.rs:227`; `apps/platform_fixture/wamn.k:130`.  
Problem: The tests freeze a growing fixture into nineteen public grants and eighteen grants after one operation becomes private. The fixture now includes `feed_line.screen`, which the frozen grant list omits. Both tests are quarantined, including the security assertion that a private operation receives no grant.  
Proposal: Use a small, owned manifest with an explicit public/private operation set. Assert exact membership and the effect of changing visibility independently of the shared application fixture.  
Deletes: The nineteen-entry shared-fixture snapshot, the absolute eighteen-grant assertion, and the `wamn-e1zd9` and `wamn-slf04` ignores.  
Risk: A private operation can acquire a public permission without default coverage. `private_custom_operations_never_become_role_grants` must reject a mutant that grants its private operation.

## Finding G2-3: Four-table pin disables the introspection refusal suite

Severity: medium  
Effort: S  
Area: `crates/schema/introspection/tests/postgres_live.rs`  
Evidence: `crates/schema/introspection/tests/postgres_live.rs:423`; `crates/schema/introspection/tests/postgres_live.rs:427`; `crates/schema/introspection/tests/postgres_live.rs:1357`; `crates/schema/introspection/tests/postgres_live.rs:1372`; `apps/platform_fixture/migrations/0001_initial.sql:1`.  
Problem: The test already compares the introspected relation set with PostgreSQL. It then requires exactly four tables, although the selected migration declares seven. The `wamn-fr2lg` ignore suppresses the whole scenario, including later unsupported-object and privilege refusals.  
Proposal: Keep server-to-model equality, schema exclusion, and refusal behavior. Let the fixture migration own its relation inventory.  
Deletes: The four-table assertion and quarantine attribute.  
Risk: Removing the count must preserve detection of omitted relations. `the_fixture_migration_round_trips_and_refuses_unsupported_server_objects` must execute its complete refusal matrix.

## Finding G2-4: Chart argument counts freeze incidental structure

Severity: medium  
Effort: M  
Area: `test-support/harness/src/case_release.rs`, `crates/control/lib/src/dev/activation.rs`  
Evidence: `test-support/harness/src/case_release.rs:1092`; `test-support/harness/src/case_release.rs:1115`; `crates/control/lib/src/release_chart.rs:418`; `test-support/harness/src/case_release.rs:878`; `crates/control/lib/src/dev/activation.rs:2051`.  
Problem: The case renderer appends its insecure-registry flag to five release arguments, producing six. Its quarantined test still requires five. Separately, `wamn-bjs11` suppresses local/deployed host parity, which needs Helm but also carries a recorded failure quarantine.  
Proposal: Test the registry flag and preservation of release arguments without freezing their total count. Keep a rendered parity test for explicitly shared host behavior, with Helm named as its prerequisite.  
Deletes: The five-argument assertion and both bead quarantine reasons after their causes are resolved.  
Risk: Case hosts can lose access to their HTTP registry, or local execution can diverge from deployment. `the_case_hosts_read_a_plain_http_registry` and `local_host_matches_the_deployed_host_contract` provide the required proofs.

## Finding G2-5: PostgreSQL-only ignores exclude ordinary credential tests

Severity: medium  
Effort: S  
Area: `crates/platform/runtime/src/plugins/connection_http/tests`  
Evidence: `crates/platform/runtime/src/plugins/connection_http/tests/credential_retry_tests.rs:23`; `crates/platform/runtime/src/plugins/connection_http/tests/credential_retry_tests.rs:49`; `crates/platform/runtime/src/plugins/connection_http/tests/oauth2_tests.rs:39`; `crates/platform/runtime/src/plugins/connection_http/tests/oauth2_tests.rs:459`; `test-support/postgres/lib.rs:38`.  
Problem: Two credential retry tests are ignored solely because they require PostgreSQL 18. Their fixture starts a local listener and uses the same owned database support as neighboring default tests. These reasons identify no additional infrastructure requirement.  
Proposal: Run both tests by default alongside the OAuth2 tests.  
Deletes: Both PostgreSQL-only ignore attributes.  
Risk: Credential rotation can stop retrying correctly or send an unnecessary second request. The two tests must prove their existing authorization sequences and request counts.

## Finding G2-6: Several count pins have no named contract

Severity: low  
Effort: S  
Area: `crates/schema/control`, `crates/identity/project-state`, `services/cdc-reader`  
Evidence: `crates/schema/control/src/run_plane/tests.rs:467`; `crates/identity/project-state/src/lib.rs:400`; `services/cdc-reader/src/lib.rs:2781`; `services/cdc-reader/src/lib.rs:1668`.  
Problem: Six run-plane tables, thirteen platform tables, and exactly 256 pending publications describe current implementation choices. None of these assertions names a contract that requires its exact number. The CDC reader already enforces the actual semaphore relationship with a compile-time assertion.  
Proposal: Retain complete relation comparisons, uniqueness checks, reconciliation behavior, and the semaphore bound. Preserve independently specified security sets, such as the platform-grain family set at `crates/control/provision/tests/deploy_sql_authority.rs:139`.  
Deletes: Redundant absolute inventory counts and `in_flight_bound_is_pinned_under_the_client_semaphore`.  
Risk: An incomplete inventory can escape a count-only replacement. Preserve `observation_at_record_plans_a_noop`, table uniqueness coverage, and pipeline settlement tests.

## Finding G2-7: SQL formatting receives the same freeze treatment as persisted identity

Severity: low  
Effort: S  
Area: `crates/control/provision/src/tenant_key.rs`  
Evidence: `crates/control/provision/src/tenant_key.rs:338`; `crates/control/provision/tests/tenant_key_live.rs:156`; `crates/control/provision/tests/tenant_key_live.rs:186`; `crates/control/provision/tests/tenant_key_live.rs:249`.  
Problem: `the_emitted_ddl_is_frozen` duplicates the complete generated SQL, including whitespace. Its security contract concerns derivation, ownership, privileges, and function properties. The source names no consumer that requires the entire statement text byte for byte.  
Proposal: Keep independent digest vectors and live SQL behavior. Preserve named freezes for persisted failure literals, durability values, release digests, hashed requirements, and event wire shapes. Their owners are explicit at `crates/execution/run-state/tests/store.rs:41`, `crates/execution/run-state/src/durability.rs:18`, `crates/catalog/model/tests/serving_manifest_digest.rs:128`, `crates/control/lib/src/component_admission.rs:1578`, and `apps/platform/events/registration/tests/event_reg.rs:147`.  
Deletes: The complete DDL string snapshot.  
Risk: A derivation or privilege change can break tenant access. Keep the frozen digest at `crates/execution/run-state/src/tenant_scope.rs:72` and the live derivation, function-property, and replacement-denial tests.

## Finding G2-8: Empty prerequisite reasons conceal a built-service dependency

Severity: low  
Effort: S  
Area: `services/ctl/tests/pat_bootstrap_live.rs`, `tests/integration/src/route_authentication_live.rs`  
Evidence: `services/ctl/tests/pat_bootstrap_live.rs:23`; `tests/integration/src/route_authentication_live.rs:795`; `tests/integration/src/route_authentication_live.rs:1254`; `crates/control/lib/src/dev/pat_issuer.rs:288`.  
Problem: Three ignores say only `requires:`. Their setup requires a built `wamn-identity` executable, supplied beside the executable or through `WAMN_IDENTITY_BINARY`. These are infrastructure gates, but their reasons do not explain how to satisfy them.  
Proposal: Name the built identity service and its supported discovery paths in each reason.  
Deletes: The empty prerequisite reasons and empty prerequisite calls where direct executable discovery supplies the required failure.  
Risk: Selection can fail before authentication coverage executes. The bootstrap and two route-authentication tests must run with the documented executable input.

## Open questions for the owner

1. Can root supply the current open “fails on main” beads? `bd list --readonly --status open --limit 0` failed with `Error: failed to open database: embeddeddolt: init schema: embeddeddolt: open db: failed to load database "wamn": openat LOCK: read-only file system`. Neither passive export contains the seven quarantine IDs. Automatic export is disabled at `.beads/config.yaml:69`, so their current statuses remain unverified.
2. Which shared host behaviors must the restored parity test enforce? Its broad comparison removes many deliberate differences at `crates/control/lib/src/dev/activation.rs:2234`.
3. Which current dashboard or collector owns the frozen PostgreSQL telemetry identifiers? The guard cites deleted archive documents at `tests/conformance/tests/effect_spans.rs:645`. Preserve the identifiers until that ownership is resolved.

## Proposed fix epics

### Restore quarantined behavioral proofs

1. Restore history reconstruction and retention tests
2. Replace shared-fixture grant snapshots with visibility contracts
3. Restore introspection relation equality and refusal coverage

### Separate infrastructure selection from failures

1. Restore registry rendering and host parity tests
2. Run owned PostgreSQL credential tests by default
3. Name identity prerequisites in ignored tests

### Retain contracts and delete incidental pins

1. Delete redundant inventory and publication-count pins
2. Replace complete DDL snapshots with existing semantic proofs
3. Resolve the current owner of frozen telemetry identifiers

<!-- lane G3, bead wamn-lybom.3 -->

## Part 3: tiers

## Finding G3-1: Default and ignored tests do not define useful execution tiers

Severity: high  
Effort: L  
Area: `tests/`, `crates/*/*/tests/`, `docs/operations/running-tests.md`

Evidence: `crates/execution/run-state/tests/admission_live.rs:60` runs PostgreSQL by default. `tests/system/src/deadline_test.rs:19` exercises an in-process Wasmtime engine. `tools/test-changes:398` selects every ignored test for `--cluster`. `docs/operations/running-tests.md:367` groups built components, brokers, registries, and clusters together.

Problem: The default suite includes live databases, while ignored tests span several unrelated execution costs. Package names and ignore attributes therefore reveal neither the required infrastructure nor the boundary that a passing test establishes. Selecting all ignored tests cannot provide a dependable cluster tier.

Proposal: Assign each executable test target to one tier. Split mixed targets before assigning them. Use the strongest required execution boundary, with prerequisites recorded separately.

Unit: Pure decisions, transformations, and properties with controlled time. No database, broker, network service, subprocess, or component runtime.

Contract: Interfaces, serialized values, WIT, generated artifacts, component admission, and rendered deployment objects. Files, command-line tools, and owned loopback fixtures are permitted. PostgreSQL, NATS, Kubernetes, and Google Cloud access are excluded.

Live Postgres: Tests that require actual PostgreSQL behavior, including grants, row security, transactions, locks, migrations, and durable queue storage. A private database still puts a test in this tier.

Live NATS: Tests that require an actual NATS broker, including JetStream persistence, acknowledgments, deduplication, authentication changes, and broker recovery. Tests that also require PostgreSQL belong here.

Kind cluster: Tests that require Kubernetes scheduling, operators, deployed images, Secrets, readiness, rollout, or process termination. Embedded PostgreSQL and NATS remain prerequisites of this tier.

GKE: Tests that require a real GKE installation or Google provider behavior, including Workload Identity, Artifact Registry access, and Cloud Storage. Local metadata-server fixtures remain contract tests.

Deletes: The default-versus-ignored tier model and the `--cluster` alias for every ignored test.

Risk: Moving tests can silently remove coverage. Compare discovered test identities before and after migration, and require exactly one tier assignment for every executable target.

## Finding G3-2: Selection policy is scattered across the runner

Severity: high  
Effort: M  
Area: `tools/test-changes`, `tools/test-changes-suites.json`

Evidence: `tools/test-changes-suites.json:3` adds generator dependencies. `tools/test-changes:153` embeds WIT mappings. `tools/test-changes:223` adds JetStream separately. `tools/test-changes:265` defines preparation requirements. `tools/test-changes:427` defines additional commands.

Problem: The JSON file supplies only part of the selection policy. Contributors must inspect shell branches to discover commands, prerequisites, and exceptions. For example, the native host test depends on the `prekind` path rule, which names control and deployment paths rather than every runtime owner that the host executes.

Proposal: Replace `tools/test-changes-suites.json` with `tests/selection.toml`, the sole selection policy file. Each suite records its stable identifier, tier, Cargo workspace, package, target, optional exact test names, features, prerequisites, extra input paths, execution policy, resource lock, and timeout.

Keep Cargo dependency discovery in the runner. Keep exceptional dependencies outside Cargo in the selection file, including WIT readers, generated files, SQL, charts, Dockerfile inputs, and browser assets. Both local selection and the merge train consume the same resolved plan.

Require complete target coverage, valid exact selectors, and explanations for additional path dependencies. An unknown code path selects all applicable suites rather than no tests. Documentation-only changes select source policy and documentation tools without generating every application.

Deletes: The old JSON file and suite-specific shell branches. Retain existing build commands and fixture owners, but remove their duplicated selection conditions.

Risk: An incomplete replacement can miss dependencies. Extend `tests/conformance/tests/test_changes.rs:3` with cases for runtime-to-host selection, WIT readers, deleted paths, unknown paths, and documentation-only changes.

## Finding G3-3: The merge train omits required live behavior

Severity: high  
Effort: M  
Area: `tools/train-run`, `tools/test-changes`

Evidence: `tools/train-run:80` invokes the default selection command. `tools/test-changes:398` requires a separate mode for ignored tests. `tools/test-changes:439` and `tools/test-changes:488` explicitly add only particular ignored cases. `apps/wamn_receiving/tests/route_authentication_live/local_business.rs:13` leaves business histories ignored. `docs/operations/running-tests.md:445` limits a cluster stage to changes under cluster tests.

Problem: A train can pass without executing affected application histories or broker-backed behavior. Changes to runtime, SQL, images, or charts can invalidate deployment behavior without changing a cluster test file. The existing positive-result guard establishes that some cases ran, not that every selected case ran.

Proposal: Run one resolved plan against the combined merge head. Prepare required artifacts once, then run affected unit, contract, live Postgres, and live NATS suites. Include generated-client and application histories when their inputs change.

Require kind suites for changes to deployment owners, image composition, Kubernetes lifecycle behavior, and their declared dependencies. Run Receiving followed by WMS on their existing shared delivery cluster when both are selected.

Require GKE evidence before merging Google provider or GKE infrastructure changes. Run the broader GKE smoke suite before release promotion. Ordinary unit changes do not require cloud execution.

Formatting, Clippy, repository policy, and guest compilation remain gates beside the six test tiers. Every selected suite must execute its expected cases. Missing prerequisites, absent cases, and excluded required stages fail the train.

Deletes: Manual live-stage instructions as the only execution path for required cases. Remove the rule that cluster coverage depends only on edits to cluster tests.

Risk: The train can become expensive or unavailable without owned fixtures. Prove selection through `tools/train-tools-test` and selector fixtures, then demonstrate failures from a missing prerequisite and a missing required case.

## Finding G3-4: Ignore reasons mix prerequisites, disabled tests, and child helpers

Severity: medium  
Effort: M  
Area: `docs/operations/running-tests.md`, ignored Rust tests

Evidence: `docs/operations/running-tests.md:368` requires `requires:` reasons. `services/ctl/tests/pat_bootstrap_live.rs:23` declares an empty prerequisite list. `services/ctl/tests/prune_record_history_live.rs:431` names only a bead. `crates/execution/run-state-sqlite/tests/intent_store.rs:198` marks a child helper as ignored, and line 203 returns without asserting anything when its input is absent.

Problem: The runner cannot distinguish an optional live case from a disabled assertion or an internal subprocess helper. Broad ignored-test selection can count the child helper as a passing test without exercising recovery. Free-text prerequisites also require manual interpretation.

Proposal: Keep tier assignment in `tests/selection.toml`. Reserve ignore reasons for prerequisites, with a fixed form such as `requires: bin:nats-server, env:WAMN_FLOW_HTTP_COMPONENT`. Record artifact preparation in the selection file, and name required artifact variables in the reason.

Move the SQLite child helper into a support executable that only its parent test invokes. Repair bead-only disabled tests or delete them after identifying replacement coverage. During migration, any temporary exclusion must name its bead, owner, and expiry in the selection file. Missing inputs must fail before setup.

Deletes: Empty reasons, prose-only prerequisite lists, permanent bead-only ignores, and the ignored SQLite helper test.

Risk: Deleting disabled tests can remove unique assertions. Preserve the parent `reopen_after_kill_finds_the_uncertain_intent` case and require selector tests to reject malformed reasons and unintended helpers.

## Finding G3-5: Generated output prevents isolated contract compilation

Severity: medium  
Effort: M  
Area: `crates/client/core/tests/`, `crates/schema/generator/tests/`

Evidence: `crates/client/core/tests/request.rs:72` and `crates/client/core/tests/captured.rs:205` include generated route schemas at compile time. `crates/schema/generator/tests/platform_generation.rs:1812` includes generated SQL. Measurements report 16 missing-generated-file compilation errors in measurements.md section 3. `tools/test-changes:293` prepares application inputs for the quality suite.

Problem: Small client and generator assertions cannot compile from a clean checkout without unrelated application generation. Because Clippy compiles these targets, even documentation changes acquire database-backed preparation. Ignoring a test does not remove its compile-time file dependency.

Proposal: Separate client behavior from generator-to-client compatibility. Give request-construction tests small explicit schemas. Keep one contract test that consumes the current generated fixture and proves agreement with the client.

Generate compatibility artifacts through an explicit preparation step and read them at runtime. Tests that require database introspection belong in live Postgres. Keep unit and contract targets independently compilable without `apps/target/wamn`.

Deletes: Compile-time reads of generated application output from ordinary test targets. Remove application generation from documentation-only quality execution.

Risk: Handwritten schemas alone can drift from generated contracts. Preserve a generated-schema compatibility case that proves omission and explicit-null behavior against current generator output.

## Finding G3-6: Tests need target moves and narrower deployment assertions

Severity: medium  
Effort: L  
Area: `tests/system/`, `crates/control/lib/tests/`, application tests

Evidence: `tests/system/src/deadline_test.rs:25` constructs a local engine. `crates/control/lib/tests/platform_apply_kind.rs:2169` renders deployment objects without a cluster. `crates/control/lib/tests/support/prekind_render.rs:68` does the same for environments. `apps/wamn_receiving/tests/route_authentication_live/local_business.rs:24` uses the shared local runtime. `docs/architecture/overview.md:245` states that kind omits Google provider implementations.

Problem: Mixed targets make cheap assertions travel with expensive fixtures. Repeating complete business histories at deployment boundaries increases cost without identifying which deployment property needs the repetition. Conversely, kind evidence cannot establish Google provider access.

Proposal: Move the epoch-interruption case into an engine-owned contract target. Move both prekind render groups and Helm-only render cases into contract targets. Move `prekind_host` into live NATS because it requires a broker, PostgreSQL, and built guests.

Move admission authority and schedule storage cases into live Postgres targets. Keep paused-time scheduling decisions in unit targets. Assign route-interface, Receiving histories, WMS local histories, and broker recovery to live NATS when they require the shared runtime broker.

Keep complete business histories in application owners. At kind boundaries, retain deployed authentication, artifact loading, isolation, rollout, restart, and recovery assertions. Reuse existing history functions where a deployed history remains necessary.

Merge repeated delivery setup into the existing shared harness and kept-cluster lifecycle. Preserve application assertions and the Receiving-then-WMS ordering. Keep actual Google authorization and storage assertions in GKE.

Deletes: Mixed render-and-cluster compilation targets, duplicate delivery setup, and repeated business assertions that prove no additional deployed boundary. Measurements.md section 7 reports 201 duplicated lines between Receiving and WMS tests, which supports inspecting this merge.

Risk: Removing deployment assertions can hide packaging failures. Retain `owned_release_delivery`, `environment_apply_kind`, and deployed restart cases as named kind requirements. Require separate GKE evidence for provider access.

## Finding G3-7: Tier durations lack a retained baseline

Severity: low  
Effort: S  
Area: `tools/train-run`, `docs/operations/running-tests.md`

Evidence: `tools/train-run:34` records step timestamps, while line 60 deletes successful logs. `docs/operations/running-tests.md:460` requests separate build and execution timing. Line 790 describes approximately 30 minutes per kind case. Line 1386 allows approximately three minutes for each of two live schedule cases.

Problem: The shared measurements contain no tier execution baseline. Existing durations establish that some cases are expensive, but they do not establish an expected train duration. Combined build and test timing also conceals whether selection or compilation causes delay.

Proposal: Use these initial planning ranges for one selected suite with cached builds and prepared artifacts. These are proposed budgets, not measured results.

Unit: 10–60 seconds.

Contract: 1–3 minutes.

Live Postgres: 3–8 minutes, including private database setup and cleanup.

Live NATS: 3–10 minutes, including required private PostgreSQL and broker setup.

Kind cluster: 30–60 minutes for one complete scenario, including cluster setup and cleanup.

GKE: 10–30 minutes against an already prepared disposable environment. Record cloud provisioning separately.

Store a compact result outside the checkout with the revision, selection-file digest, selected and executed cases, and preparation, execution, and cleanup durations. After collecting a baseline on the agreed machine, replace estimates with measured ranges.

Deletes: Successful-result deletion as the only retention policy. Replace scattered timing expectations with the selection file’s budgets.

Risk: Tight budgets can reject correct behavior on slower machines. Retain the real schedule-lock cases and full kind scenarios when calibrating budgets.

## Open questions for the owner

1. Which disposable GKE project and existing fixtures can support required provider tests before merge?
2. Which bead-only ignored tests represent current defects, and which represent retired contracts?
3. Which machine and cache state define the timing baseline?

## Proposed fix epics

### Test tiers and selection

1. Split mixed test targets into the six execution tiers
2. Replace suite mappings and shell exceptions with tests/selection.toml
3. Normalize prerequisites and remove ignored subprocess helpers

### Merge evidence and execution cost

1. Execute required live and deployment suites on the combined merge head
2. Isolate generated contracts and merge shared delivery setup
3. Retain execution counts and calibrate tier duration budgets

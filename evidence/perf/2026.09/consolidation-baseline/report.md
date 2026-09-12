# Consolidation starting state

The retained workspace sweep (tests across all root packages) failed with exit 101 at `a0e833b9398c829030a055f5c38755aebd98d358` on 2026-09-11.
It reported 84 failed tests across 39 test targets.
This report records the starting state for the [consolidation charter](../../../../docs/architecture/consolidation-findings.md).
Bead `wamn-47wm.1` owns this measurement (`bd show wamn-47wm.1`).

The run reported 2,245 test passes and six doctest passes (tests in Rust documentation).
Those passes include 85 explicit self-skips, which are successful returns without execution of the required proof.
The [named skip records](run-001/classified-failures.json) retain each test, message, required input, and raw log line.
This count is a lower bound because silent early returns can escape the log.
The runner armed no live prerequisites, so reported passes do not establish live application proof.

## Source and command

The integrated tree named the same HEAD at both source captures.
The tree contained Beads export changes, the untracked charter, existing untracked performance evidence, and `docs/poc/generated-tui-spec.md`.
The [before receipt](run-001/source-before.json) and [after receipt](run-001/source-after.json) retain the full observed Git status.
They record matching combined hashes for 25,832 tracked files, based on contents and modes, with Beads excluded.
The [source comparison](run-001/source-stability.json) lists no changed tracked inputs.
These receipts make no claim that the tree was clean.

The run reused the unchanged [existing runner](../generated-tui-integration/tools/workspace.py):

```bash
python3 docs/perf/2026.09/generated-tui-integration/tools/workspace.py \
  --evidence-dir docs/perf/2026.09/consolidation-baseline/run-001
```

The runner used the [retained command](run-001/command.json):

```bash
cargo test --workspace --locked --offline --no-fail-fast -- \
  --include-ignored --nocapture --test-threads=1 \
  --skip regenerate_checked_in_journey_schema \
  --skip regenerate_checked_in_dev_config_schema
```

The runner captured complete, unpiped output in [workspace.log](run-001/workspace.log) and preserved [exit 101](run-001/exit-code.txt).
It selected all root workspace members, included ignored tests, and excluded only the two named schema regeneration tests.
The output contains 194 test targets and 37 doctest targets.
The [run receipt](run-001/run.json) records 140.679 seconds between the command and exit receipts.
This interval includes temporary-directory setup and cleanup and is not benchmark timing.

The runner used Rust 1.98.0, two Cargo build jobs, and the existing root target directory.
It removed `CARGO_TARGET_DIR` and ambient `WAMN_`, `PG`, `OTEL_`, `GIT_`, and `DATABASE_URL` inputs.
It used private temporary and Helm directories and set `KUBECONFIG=/dev/null`.
A host process inspection found no concurrent Cargo or Rust compiler process before launch.
The [environment receipt](run-001/environment-names.json) records input names and the controls, without credential values.
The run did not arm a database, broker, registry, cluster, or artifact fixture.

## Classified failures

All 84 failures remain failures.
The classifications below identify 81 missing live or artifact inputs, one WIT scan defect, one unavailable Kubernetes discovery endpoint, and one deployment prerequisite mismatch.
Each row names one failed test and links its diagnostic in the raw log.
The [full classifications](run-001/classified-failures.json) retain the causes, required inputs, source references, and named skips.

| Package and Cargo target | Failed test | Class | Cause | Raw log |
|---|---|---|---|---|
| `wamn-catalog --test wiring_activation_live` | `the_terminal_document_reaches_a_converged_database_and_survives_the_column` | Missing input | The runner does not set `WAMN_CATALOG_PG_URL`. | [L541](run-001/workspace.log#L541) |
| `wamn-catalog --test wiring_activation_live` | `wiring_activation_live` | Missing input | The runner does not set `WAMN_CATALOG_PG_URL`. | [L546](run-001/workspace.log#L546) |
| `wamn-control-provision --test control_portable_store` | `current_database_connect_posture_is_exactly_scoped` | Missing input | The runner does not set `WAMN_CONTROL_PORTABLE_PG_URL`. | [L1034](run-001/workspace.log#L1034) |
| `wamn-control-provision --test identity_issuer_live` | `scoped_issuer_grants_and_generation_retirement_execute_on_postgres` | Missing input | The runner does not set `WAMN_IDENTITY_ISSUER_PG_URL`. | [L1157](run-001/workspace.log#L1157) |
| `wamn-control-provision --test session_role_reader_live` | `dedicated_session_reader_columns_and_generations_execute_on_postgres` | Missing input | The runner does not set `WAMN_SESSION_ROLE_READER_PG_URL`. | [L1212](run-001/workspace.log#L1212) |
| `wamn-ctl --lib` | `dev::verification_database::tests::disposable_postgres_proves_freshness_cleanup_and_confinement` | Missing input | The runner does not set `WAMN_DEV_VERIFICATION_PG_URL`. | [L1439](run-001/workspace.log#L1439) |
| `wamn-ctl --lib` | `dev::verification_world::tests::lifecycle_bootstraps_then_accepts_packages_and_one_exact_admission` | Missing input | The runner does not set `WAMN_DEV_VERIFICATION_PG_URL`. | [L1450](run-001/workspace.log#L1450) |
| `wamn-ctl --lib` | `publish_release::effective_release_live::fresh_base_and_overlay_mint_byte_identically_and_refuse_drift` | Missing input | The runner does not set `WAMN_EFFECTIVE_RELEASE_PROJECT_PG_URL`. | [L1545](run-001/workspace.log#L1545) |
| `wamn-ctl --lib` | `push_component::tests::production_publisher_and_puller_round_trip_exact_bytes` | Missing input | The runner does not set `WAMN_COMPONENT_ARTIFACT_BASE`. | [L1575](run-001/workspace.log#L1575) |
| `wamn-ctl --lib` | `push_release_manifest::tests::production_publisher_exact_retry_is_a_no_push` | Missing input | The runner does not set `WAMN_RELEASE_MANIFEST_ARTIFACT_BASE`. | [L1593](run-001/workspace.log#L1593) |
| `wamn-ctl --test author_wiring_gate_report_live` | `a_wiring_is_authored_only_under_a_green_report_for_its_own_hash` | Missing input | The runner does not set `WAMN_AUTHOR_WIRING_PROJECT_PG_URL`. | [L1663](run-001/workspace.log#L1663) |
| `wamn-ctl --test bind_connection_live` | `bind_connection_round_trips_through_the_plugins_own_resolution` | Missing input | The runner does not set `WAMN_BIND_CONNECTION_PROJECT_PG_URL`. | [L1680](run-001/workspace.log#L1680) |
| `wamn-ctl --test effect_writer_generation_live` | `effect_writer_generation_lifecycle_is_exact_and_fail_closed` | Missing input | The runner does not set `WAMN_EFFECT_WRITER_PG18_URL`. | [L1718](run-001/workspace.log#L1718) |
| `wamn-ctl --test identity_issuer_live` | `compiled_cli_publishes_rolls_back_and_retires_identity_generations` | Missing input | The runner does not set `WAMN_IDENTITY_ISSUER_CLI_PG_URL`. | [L1743](run-001/workspace.log#L1743) |
| `wamn-ctl --test management_admitter_generation_live` | `management_admitter_generation_lifecycle_converges_and_rotates` | Missing input | The runner does not set `WAMN_MANAGEMENT_ADMITTER_PG18_URL`. | [L1761](run-001/workspace.log#L1761) |
| `wamn-ctl --test pat_bootstrap_live` | `cli_bootstrap_mints_first_service_pats_over_https` | Missing input | The runner does not set `WAMN_PAT_BOOTSTRAP_ALLOW_SCHEMA_RESET`. | [L1790](run-001/workspace.log#L1790) |
| `wamn-ctl --test run_plane_live` | `effect_writer_cutover_live` | Missing input | The runner does not set `WAMN_CTL_PG_URL`. | [L1841](run-001/workspace.log#L1841) |
| `wamn-ctl --test run_plane_live` | `failure_detail_cutover_live` | Missing input | The runner does not set `WAMN_CTL_PG_URL`. | [L1848](run-001/workspace.log#L1848) |
| `wamn-ctl --test run_plane_live` | `frame_identity_cutover_live` | Missing input | The runner does not set `WAMN_CTL_PG_URL`. | [L1852](run-001/workspace.log#L1852) |
| `wamn-ctl --test run_plane_live` | `partition_plane_active_lease_refusal_live` | Missing input | The runner does not set `WAMN_CTL_PG_URL`. | [L1856](run-001/workspace.log#L1856) |
| `wamn-ctl --test run_plane_live` | `partition_plane_cutover_live` | Missing input | The runner does not set `WAMN_CTL_PG_URL`. | [L1860](run-001/workspace.log#L1860) |
| `wamn-ctl --test run_plane_live` | `provisioner_minted_generation_live` | Missing input | The runner does not set `WAMN_CTL_PG_URL`. | [L1864](run-001/workspace.log#L1864) |
| `wamn-ctl --test run_plane_live` | `two_plane_residency_live` | Missing input | The runner does not set `WAMN_CTL_PG_URL`. | [L1880](run-001/workspace.log#L1880) |
| `wamn-ctl --test session_audience_live` | `compiled_cli_publishes_bound_session_targets_and_rotates_reader_generations` | Missing input | The runner does not set `WAMN_SESSION_AUDIENCE_CLI_PG_URL`. | [L1902](run-001/workspace.log#L1902) |
| `wamn-execution-host --lib` | `router_driver::native_policy::tests::authenticated::native_authenticated_nested_authority_and_lifecycle` | Missing input | The runner does not set `WAMN_NATIVE_B_AUTH_PG_URL`. | [L2006](run-001/workspace.log#L2006) |
| `wamn-host --test native_lifecycle_live` | `rebuilt_host_probes_signals_and_scheduler_recovery` | Missing input | The runner does not set `WAMN_HOST_LIVE_NATS_SERVER_BIN`. | [L2168](run-001/workspace.log#L2168) |
| `wamn-identity --test https_surface` | `identity_https_has_only_public_jwks_and_health` | Missing input | The runner does not set `WAMN_IDENTITY_SERVICE_ALLOW_SCHEMA_RESET`. | [L2199](run-001/workspace.log#L2199) |
| `wamn-identity --test pat_issuance` | `operator_pat_issuance_over_https` | Missing input | The runner does not set `WAMN_PAT_ISSUANCE_ALLOW_SCHEMA_RESET`. | [L2219](run-001/workspace.log#L2219) |
| `wamn-identity --test session_exchange` | `session_exchange_uses_fresh_scoped_authority_without_session_state` | Missing input | The runner does not set `WAMN_SESSION_EXCHANGE_ALLOW_SCHEMA_RESET`. | [L2236](run-001/workspace.log#L2236) |
| `wamn-platform-identity --test session_keys_live` | `session_key_lifecycle_on_postgres` | Missing input | The runner does not set `WAMN_SESSION_KEYS_ALLOW_SCHEMA_RESET`. | [L2304](run-001/workspace.log#L2304) |
| `wamn-platform-identity --test session_keys_live` | `signer_backend_loss_before_commit_does_not_issue_token` | Missing input | The runner does not set `WAMN_SESSION_KEYS_ALLOW_SCHEMA_RESET`. | [L2311](run-001/workspace.log#L2311) |
| `wamn-proof-conformance --lib` | `version_identity::wamn_wit_packages_stay_at_mvp_version` | WIT scan defect | The scan compares `0.1.0 {` with `0.1.0` in eight retained WIT declarations. | [L2451](run-001/workspace.log#L2451) |
| `wamn-proof-conformance --test chart_seam_governance` | `receiving_pat_overlay_renders_a_complete_scoped_host` | Unavailable service | Kubernetes discovery at `localhost:8080` refuses the connection with `KUBECONFIG=/dev/null`. | [L2491](run-001/workspace.log#L2491) |
| `wamn-proof-conformance --test guest_workspace_closure` | `one_commit_built_in_two_checkouts_yields_identical_guest_digests` | Missing input | The runner does not set `WAMN_DIGEST_REPRO_A`. | [L2583](run-001/workspace.log#L2583) |
| `wamn-proof-conformance --test guest_workspace_closure` | `one_commit_built_under_two_profiles_yields_identical_guest_digests` | Missing input | The runner does not set `WAMN_DIGEST_PROFILE_M1_PLAN`. | [L2588](run-001/workspace.log#L2588) |
| `wamn-proof-integration --lib` | `claim_law_live::a_changed_request_under_a_live_key_refuses_with_idempotency_conflict` | Missing input | The runner does not set `WAMN_CLAIM_LAW_PG_URL`. | [L2808](run-001/workspace.log#L2808) |
| `wamn-proof-integration --lib` | `claim_law_live::a_claim_that_updates_on_conflict_fails_both_emitted_cases` | Missing input | The runner does not set `WAMN_CLAIM_LAW_PG_URL`. | [L2813](run-001/workspace.log#L2813) |
| `wamn-proof-integration --lib` | `claim_law_live::a_replay_returns_the_immutable_original_result_and_writes_nothing` | Missing input | The runner does not set `WAMN_CLAIM_LAW_PG_URL`. | [L2818](run-001/workspace.log#L2818) |
| `wamn-proof-integration --lib` | `hot_route_trace::tests::an_incoming_traceparent_reaches_the_outbound_socket_under_the_same_trace` | Missing input | The runner does not set `WAMN_HOTROUTE_PG_URL`. | [L2828](run-001/workspace.log#L2828) |
| `wamn-proof-integration --lib` | `provisionbench::tests::legacy_converges_database_authority_on_postgres` | Missing input | The runner does not set `WAMN_PG_ADMIN_URL`. | [L2840](run-001/workspace.log#L2840) |
| `wamn-proof-integration --lib` | `provisionbench::tests::project_env_replay_uses_the_stored_instance_suffix` | Missing input | The runner does not set `WAMN_PG_ADMIN_URL`. | [L2844](run-001/workspace.log#L2844) |
| `wamn-proof-integration --lib` | `receiving_data_access::tests::enum_and_optimistic_update_outcomes_hold_on_postgres_18` | Missing input | The runner does not set `WAMN_RECEIVING_PG_URL`. | [L2848](run-001/workspace.log#L2848) |
| `wamn-proof-integration --lib` | `receiving_data_access::tests::generated_update_ignores_ungranted_additive_columns` | Missing input | The runner does not set `WAMN_RECEIVING_PG_URL`. | [L2853](run-001/workspace.log#L2853) |
| `wamn-proof-integration --lib` | `route_authentication_live::fresh_only::execution_tests::counter_uses_login_tenant_without_a_guest_guc` | Missing input | The runner does not set `WAMN_TENANT_KEY_PG_URL`. | [L2863](run-001/workspace.log#L2863) |
| `wamn-proof-integration --lib` | `route_authentication_live::overlay_compatibility::installed_contract_observer_preserves_acls_and_refuses_changed_requirements` | Missing input | The runner does not set `WAMN_OVERLAY_OBSERVER_DATABASE_URL`. | [L2871](run-001/workspace.log#L2871) |
| `wamn-proof-integration --lib` | `route_authentication_live::postcommit::production_materializer_preserves_replay_and_progress` | Missing input | The runner does not set `WAMN_JOURNEY_DOCUMENT`. | [L2874](run-001/workspace.log#L2874) |
| `wamn-proof-integration --lib` | `route_authentication_live::product_dev_command_owns_the_clean_twelve_stage_receipt_and_cleanup` | Missing input | The runner does not set `WAMN_ROUTE_PG18_URL`. | [L2876](run-001/workspace.log#L2876) |
| `wamn-proof-integration --lib` | `route_authentication_live::production_materializer_consumes_the_causal_receipt_exactly_once` | Missing input | The runner does not set `WAMN_JOURNEY_DOCUMENT`. | [L2878](run-001/workspace.log#L2878) |
| `wamn-proof-integration --lib` | `route_authentication_live::production_nested_fresh_only_requires_pat_and_observes_revocation` | Missing input | The runner does not set `WAMN_JOURNEY_DOCUMENT`. | [L2880](run-001/workspace.log#L2880) |
| `wamn-proof-integration --lib` | `route_authentication_live::production_nested_session_call_preserves_original_caller` | Missing input | The runner does not set `WAMN_JOURNEY_DOCUMENT`. | [L2882](run-001/workspace.log#L2882) |
| `wamn-proof-integration --lib` | `route_authentication_live::production_receiving_session_host_fixture` | Missing input | The runner does not set `WAMN_JOURNEY_DOCUMENT`. | [L2884](run-001/workspace.log#L2884) |
| `wamn-proof-integration --lib` | `route_authentication_live::production_route_caller_authentication_and_operation_authorization` | Missing input | The runner does not set `WAMN_ROUTE_AUTH_PG18_URL`. | [L2886](run-001/workspace.log#L2886) |
| `wamn-proof-integration --lib` | `route_authentication_live::production_session_client_login_and_fresh_selection` | Missing input | The runner does not set `WAMN_JOURNEY_DOCUMENT`. | [L2890](run-001/workspace.log#L2890) |
| `wamn-proof-integration --lib` | `route_authentication_live::production_two_package_fresh_only_fixture_serves_all_thirteen_pat_routes` | Missing input | The runner does not set `WAMN_JOURNEY_DOCUMENT`. | [L2892](run-001/workspace.log#L2892) |
| `wamn-proof-integration --lib` | `route_authentication_live::production_two_package_release_serves_all_thirteen_pat_routes` | Missing input | The runner does not set `WAMN_JOURNEY_DOCUMENT`. | [L2894](run-001/workspace.log#L2894) |
| `wamn-proof-integration --lib` | `router_tap_live::tests::the_released_router_bridge_emits_accepted_and_settled_previews` | Missing input | The runner does not set `WAMN_ROUTER_TAP_NATS_URL`. | [L2897](run-001/workspace.log#L2897) |
| `wamn-proof-integration --lib` | `throughput_bench_live::tests::every_layer_ran_the_whole_sweep_and_its_knee_is_recorded` | Missing input | The runner does not set `WAMN_THROUGHPUT_EVIDENCE_DIR`. | [L2908](run-001/workspace.log#L2908) |
| `wamn-proof-integration --lib` | `trusted_http_route::tests::nested_http_authorizes_child_and_preserves_original_caller` | Missing input | The runner does not set `WAMN_HTTP_REUSE_ALLOW_SCHEMA_RESET`. | [L2912](run-001/workspace.log#L2912) |
| `wamn-proof-integration --lib` | `trusted_http_route::tests::real_http_guest_reuses_connections_without_reusing_authority` | Missing input | The runner does not set `WAMN_HTTP_REUSE_ALLOW_SCHEMA_RESET`. | [L2914](run-001/workspace.log#L2914) |
| `wamn-proof-integration --lib` | `virtualized_std_guest::tests::virtualized_artifacts_have_exact_imports_and_receiving_exports` | Missing input | The runner does not set `WAMN_STD_VIRTUALIZATION_COMPONENT_WASM`. | [L2916](run-001/workspace.log#L2916) |
| `wamn-proof-integration --lib` | `virtualized_std_guest::tests::virtualized_std_guest_hides_the_sentinel_and_maps_a_panic_to_a_typed_refusal` | Missing input | The runner does not set `WAMN_STD_VIRTUALIZATION_SENTINEL`. | [L2918](run-001/workspace.log#L2918) |
| `wamn-proof-integration --lib` | `wms_runtime_live::committed_move_survives_label_store_failure` | Missing input | The runner does not set `WAMN_JOURNEY_DOCUMENT`. | [L2924](run-001/workspace.log#L2924) |
| `wamn-proof-integration --lib` | `wms_runtime_live::contention_and_replay_through_the_composed_route` | Missing input | The runner does not set `WAMN_JOURNEY_DOCUMENT`. | [L2926](run-001/workspace.log#L2926) |
| `wamn-proof-integration --lib` | `wms_runtime_live::the_remaining_operations_serve_their_released_routes` | Missing input | The runner does not set `WAMN_JOURNEY_DOCUMENT`. | [L2931](run-001/workspace.log#L2931) |
| `wamn-proof-integration --test receiving_command_histories_live` | `production_receiving_command_histories` | Missing input | The runner does not set `WAMN_RECEIVING_CORRECTNESS_DOCUMENT`. | [L3002](run-001/workspace.log#L3002) |
| `wamn-proof-integration --test startup_burst_live` | `production_http_start_burst_keeps_native_host_progress` | Missing input | The runner does not set `WAMN_STARTUP_BURST_INPUT`. | [L3017](run-001/workspace.log#L3017) |
| `wamn-proof-system --test deploy_platform_inventory` | `every_mounted_secret_is_declared_here_or_named_a_prerequisite` | Deployment prerequisite mismatch | `values-host-wms-pat.yaml` mounts `wamn-object-store-credentials-acme--wms--dev`, which the test does not find among declared prerequisites. | [L3042](run-001/workspace.log#L3042) |
| `wamn-run-state --test admission_live` | `surviving_authority_matrix_live` | Missing input | The runner does not set `WAMN_RUN_STORE_PG_URL`. | [L3273](run-001/workspace.log#L3273) |
| `wamn-run-state --test effect_writer_live` | `native_effect_writer_live` | Missing input | The runner does not set `WAMN_RUN_STORE_PG_URL`. | [L3290](run-001/workspace.log#L3290) |
| `wamn-run-state --test run_state_live` | `run_state_live` | Missing input | The runner does not set `WAMN_RUN_STORE_PG_URL`. | [L3330](run-001/workspace.log#L3330) |
| `wamn-runtime --lib` | `plugins::wamn_jetstream::tests::live_generic_guest_cannot_write_the_provisioned_tap_stream` | Missing input | The runner does not set `WAMN_ROUTER_TAP_NATS_URL`. | [L3567](run-001/workspace.log#L3567) |
| `wamn-runtime --lib` | `plugins::wamn_postgres::claims::tests::live_size_one_guest_and_platform_pools_isolate_sessions_under_interleaving` | Missing input | The runner does not set `WAMN_POOL_LIFECYCLE_PG_URL`. | [L3620](run-001/workspace.log#L3620) |
| `wamn-runtime --lib` | `plugins::wamn_postgres::types::tests::live_a_timestamptz_read_back_spells_exactly_what_the_canonicalizer_spells` | Missing input | The runner does not set `WAMN_CARRIER_SPELLING_PG_URL`. | [L3689](run-001/workspace.log#L3689) |
| `wamn-runtime --lib` | `plugins::wamn_postgres::types::tests::live_every_carrier_that_passes_a_typed_value_as_text_matches_its_canonicalizer` | Missing input | The runner does not set `WAMN_CARRIER_SPELLING_PG_URL`. | [L3693](run-001/workspace.log#L3693) |
| `wamn-runtime --test executor_platform_surface_live` | `executor_platform_surface_live` | Missing input | The runner does not set `WAMN_EXEC_PLATFORM_PG_URL`. | [L3778](run-001/workspace.log#L3778) |
| `wamn-runtime --test production_claim_durable_live` | `production_claim_durable_live` | Missing input | The runner does not set `WAMN_DURABLE_TIER_PG_URL`, `WAMN_PRODUCTION_CLAIM_PG_URL`. | [L3873](run-001/workspace.log#L3873) |
| `wamn-runtime --test production_claim_live` | `production_claim_live` | Missing input | The runner does not set `WAMN_PRODUCTION_CLAIM_PG_URL`. | [L3890](run-001/workspace.log#L3890) |
| `wamn-runtime --test release_manifest_source` | `a_published_release_pulls_back_byte_exact_and_welds_the_release_it_names` | Missing input | The runner does not set `WAMN_RELEASE_MANIFEST_ARTIFACT_BASE`. | [L3921](run-001/workspace.log#L3921) |
| `wamn-runtime --test session_route_authentication` | `sessions_use_one_fresh_scoped_permission_union_and_preserve_the_signed_identity` | Missing input | The runner does not set `WAMN_SESSION_ROUTE_PG18_URL`. | [L3961](run-001/workspace.log#L3961) |
| `wamn-runtime --test sqlx_transaction_live` | `sqlx_command_commits_rolls_back_and_obeys_current_user_rls` | Missing input | The runner does not set `WAMN_SQLX_TRANSACTION_PG_URL`. | [L3991](run-001/workspace.log#L3991) |
| `wamn-runtime --test wiring_doorbell_live` | `a_pointer_flip_makes_the_cache_serve_the_new_active_version` | Missing input | The runner does not set `WAMN_CATALOG_PG_URL`. | [L4008](run-001/workspace.log#L4008) |
| `wamn-scenario-worker --test management_live` | `management_surface_authenticates_and_attributes_authoring_commands` | Missing input | The runner does not set `WAMN_PLATFORM_IDENTITY_PG_URL`. | [L4090](run-001/workspace.log#L4090) |
| `wamn-schema-introspection --test postgres_live` | `an_exclusion_constraint_is_modelled_rather_than_refused` | Missing input | The runner does not set `WAMN_SCHEMA_INTROSPECTION_PG_URL`. | [L4514](run-001/workspace.log#L4514) |
| `wamn-schema-introspection --test postgres_live` | `receiving_migration_round_trips_and_refuses_unsupported_server_objects` | Missing input | The runner does not set `WAMN_SCHEMA_INTROSPECTION_PG_URL`. | [L4519](run-001/workspace.log#L4519) |

## Comparison limits

The [comparison with retained Native B evidence](run-001/native-b-reference-comparison.json) matches all 84 failed test identities and normalized causes.
The reference ran at `07c7a858c4452579b12e0ae25a4e61af2107c4eb`.
The target names and explicit skip names also match, with no added or absent failures.
This comparison does not convert the current failures into successful proofs.

The unchanged reducer omits the nested child diagnostic for `native_authenticated_nested_authority_and_lifecycle`.
The current classification uses the [actual child refusal](run-001/workspace.log#L2007), which requires `WAMN_NATIVE_B_AUTH_PG_URL`.
The raw reducer output remains intact in [workspace-results.json](run-001/workspace-results.json).
Its historical cutover labels do not replace the current classifications above.

The root sweep does not run every guest build or every application journey.
The run used no release build, disposable cluster, or benchmark.
The charter requires later steps to retain application behavior and compare their integrated sweep results with this starting state.

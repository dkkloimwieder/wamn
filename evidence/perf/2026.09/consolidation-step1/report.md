# Consolidation step 1

The retained workspace sweep failed with exit 101 at `b1d40d80558cb58ea4c9e4580d0f46e7edf2fc5b` on 2026-09-11.
It recorded 84 failures and 85 explicit self-skips.
Every baseline failure identity and explicit skip remains.
The only changed cause names the new digest input, `WAMN_DIGEST_PROFILE_APP_PLAN`, in place of `WAMN_DIGEST_PROFILE_M1_PLAN`.
The [starting baseline](../consolidation-baseline/report.md) remains the comparison source.

The source contents, file modes, and HEAD stayed unchanged during the sweep.
The command and environment names match the baseline.
The runner armed no live inputs.
The run took 244.32 seconds, which records execution time rather than a performance measurement.
A self-skip reports success without executing its required proof.
The explicit skip count remains a lower bound.

The code commits remove duplicate package, tier, feature, and WIT location inventories.
Application builds read the named manifests, and proof builds read Cargo workspace membership.
Each guest keeps a separate Cargo invocation.
The build command rejects changed artifact plans before changing outputs and preserves other application outputs during an app build.
The [charter](../../../../docs/history/consolidation-findings.md) retains the owner decisions.

| Bead | Code commit | Change |
| --- | --- | --- |
| `wamn-47wm.2.1` | `b1d40d80` | Delete package and tier inventories and retire `m1` with its callers. |
| `wamn-47wm.2.2` | `e3c4e79f` | Delete feature inventories and retain runtime policy assertions. |
| `wamn-47wm.2.3` | `8a14661a` | Discover WIT copies and compare complete contract bytes. |

The exact retained runner command was:

```bash
python3 docs/perf/2026.09/generated-tui-integration/tools/workspace.py \
  --evidence-dir docs/perf/2026.09/consolidation-step1/run-001
```

The [run receipts](run-001/validation.json) link source stability, the unchanged command, and the raw log hash.
The [failure classifications](run-001/classified-failures.json) retain every cause and explicit skip.
The [baseline comparison](run-001/baseline-comparison.json) compares identities and normalized causes.
The [test changes](run-001/test-case-delta.json) account for removed, renamed, and extracted cases.
Reported test passes fell from 2,245 to 2,214 after inventory assertions were removed.
The six reported doctest passes remain unchanged.

Ten direct source lint tests and twelve build-command smoke cases passed before integration.
The WIT lane passed five tests, and deliberate contract byte changes caused three named failures.
The integrated runtime WIT Cargo targets also passed, including the connection binding compilation.
A later focused conformance build was stopped during redundant dependency compilation.
The retained workspace sweep executed those conformance tests and compiled the updated developer-loop callers.
These results do not establish live application execution or armed digest comparison.

| Measure | Baseline | Stage 1 |
| --- | ---: | ---: |
| Test targets | 194 | 191 |
| Reported test passes, including explicit skips | 2245 | 2214 |
| Test failures | 84 | 84 |
| Explicit self-skips | 85 | 85 |
| Filtered regeneration tests | 2 | 2 |
| Doctest targets | 37 | 37 |
| Reported doctest passes | 6 | 6 |

| Failure class | Count |
| --- | ---: |
| missing_live_or_artifact_input | 81 |
| source_scan_misparses_retained_wit_evidence | 1 |
| unavailable_kubernetes_discovery | 1 |
| deployment_prerequisite_declaration_mismatch | 1 |

| Package and Cargo target | Test | Classification | Actual cause | Raw log lines |
| --- | --- | --- | --- | --- |
| wamn-catalog --test wiring_activation_live | the_terminal_document_reaches_a_converged_database_and_survives_the_column | missing_live_or_artifact_input | set WAMN_CATALOG_PG_URL to the throwaway superuser database: NotPresent | [523–527](run-001/workspace.log#L523) |
| wamn-catalog --test wiring_activation_live | wiring_activation_live | missing_live_or_artifact_input | set WAMN_CATALOG_PG_URL to the throwaway superuser database: NotPresent | [528–532](run-001/workspace.log#L528) |
| wamn-control-provision --test control_portable_store | current_database_connect_posture_is_exactly_scoped | missing_live_or_artifact_input | WAMN_CONTROL_PORTABLE_PG_URL names a disposable PostgreSQL 18 database: NotPresent | [1016–1020](run-001/workspace.log#L1016) |
| wamn-control-provision --test identity_issuer_live | scoped_issuer_grants_and_generation_retirement_execute_on_postgres | missing_live_or_artifact_input | set WAMN_IDENTITY_ISSUER_PG_URL: NotPresent | [1139–1144](run-001/workspace.log#L1139) |
| wamn-control-provision --test session_role_reader_live | dedicated_session_reader_columns_and_generations_execute_on_postgres | missing_live_or_artifact_input | set WAMN_SESSION_ROLE_READER_PG_URL: NotPresent | [1194–1199](run-001/workspace.log#L1194) |
| wamn-ctl --lib | dev::verification_database::tests::disposable_postgres_proves_freshness_cleanup_and_confinement | missing_live_or_artifact_input | WAMN_DEV_VERIFICATION_PG_URL must name a disposable database: NotPresent | [1421–1425](run-001/workspace.log#L1421) |
| wamn-ctl --lib | dev::verification_world::tests::lifecycle_bootstraps_then_accepts_packages_and_one_exact_admission | missing_live_or_artifact_input | WAMN_DEV_VERIFICATION_PG_URL must name a disposable PostgreSQL 18 database: NotPresent | [1432–1435](run-001/workspace.log#L1432) |
| wamn-ctl --lib | publish_release::effective_release_live::fresh_base_and_overlay_mint_byte_identically_and_refuse_drift | missing_live_or_artifact_input | WAMN_EFFECTIVE_RELEASE_PROJECT_PG_URL names disposable PostgreSQL 18: NotPresent | [1527–1530](run-001/workspace.log#L1527) |
| wamn-ctl --lib | push_component::tests::production_publisher_and_puller_round_trip_exact_bytes | missing_live_or_artifact_input | set WAMN_COMPONENT_ARTIFACT_BASE to a disposable HTTP registry/repository: NotPresent | [1557–1560](run-001/workspace.log#L1557) |
| wamn-ctl --lib | push_release_manifest::tests::production_publisher_exact_retry_is_a_no_push | missing_live_or_artifact_input | set WAMN_RELEASE_MANIFEST_ARTIFACT_BASE to a disposable repository: NotPresent | [1575–1578](run-001/workspace.log#L1575) |
| wamn-ctl --test author_wiring_gate_report_live | a_wiring_is_authored_only_under_a_green_report_for_its_own_hash | missing_live_or_artifact_input | WAMN_AUTHOR_WIRING_PROJECT_PG_URL names a disposable PostgreSQL 18 database: NotPresent | [1645–1650](run-001/workspace.log#L1645) |
| wamn-ctl --test bind_connection_live | bind_connection_round_trips_through_the_plugins_own_resolution | missing_live_or_artifact_input | WAMN_BIND_CONNECTION_PROJECT_PG_URL names a disposable PostgreSQL 18 database: NotPresent | [1662–1667](run-001/workspace.log#L1662) |
| wamn-ctl --test effect_writer_generation_live | effect_writer_generation_lifecycle_is_exact_and_fail_closed | missing_live_or_artifact_input | set WAMN_EFFECT_WRITER_PG18_URL to a disposable PG18 superuser URL: NotPresent | [1700–1705](run-001/workspace.log#L1700) |
| wamn-ctl --test identity_issuer_live | compiled_cli_publishes_rolls_back_and_retires_identity_generations | missing_live_or_artifact_input | set WAMN_IDENTITY_ISSUER_CLI_PG_URL: NotPresent | [1725–1729](run-001/workspace.log#L1725) |
| wamn-ctl --test management_admitter_generation_live | management_admitter_generation_lifecycle_converges_and_rotates | missing_live_or_artifact_input | set WAMN_MANAGEMENT_ADMITTER_PG18_URL to a disposable PG18 superuser URL: NotPresent | [1743–1748](run-001/workspace.log#L1743) |
| wamn-ctl --test pat_bootstrap_live | cli_bootstrap_mints_first_service_pats_over_https | missing_live_or_artifact_input | assertion `left == right` failed: arm only a fresh disposable PostgreSQL server / left: Err(NotPresent) / right: Ok("1") | [1772–1779](run-001/workspace.log#L1772) |
| wamn-ctl --test run_plane_live | effect_writer_cutover_live | missing_live_or_artifact_input | WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database: NotPresent | [1823–1827](run-001/workspace.log#L1823) |
| wamn-ctl --test run_plane_live | failure_detail_cutover_live | missing_live_or_artifact_input | WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database: NotPresent | [1830–1833](run-001/workspace.log#L1830) |
| wamn-ctl --test run_plane_live | frame_identity_cutover_live | missing_live_or_artifact_input | WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database: NotPresent | [1834–1837](run-001/workspace.log#L1834) |
| wamn-ctl --test run_plane_live | partition_plane_active_lease_refusal_live | missing_live_or_artifact_input | WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database: NotPresent | [1838–1841](run-001/workspace.log#L1838) |
| wamn-ctl --test run_plane_live | partition_plane_cutover_live | missing_live_or_artifact_input | WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database: NotPresent | [1842–1845](run-001/workspace.log#L1842) |
| wamn-ctl --test run_plane_live | provisioner_minted_generation_live | missing_live_or_artifact_input | WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database: NotPresent | [1846–1849](run-001/workspace.log#L1846) |
| wamn-ctl --test run_plane_live | two_plane_residency_live | missing_live_or_artifact_input | WAMN_CTL_PG_URL must name a fresh PostgreSQL 18 database: NotPresent | [1862–1866](run-001/workspace.log#L1862) |
| wamn-ctl --test session_audience_live | compiled_cli_publishes_bound_session_targets_and_rotates_reader_generations | missing_live_or_artifact_input | set WAMN_SESSION_AUDIENCE_CLI_PG_URL: NotPresent | [1884–1888](run-001/workspace.log#L1884) |
| wamn-execution-host --lib | router_driver::native_policy::tests::authenticated::native_authenticated_nested_authority_and_lifecycle | missing_live_or_artifact_input | real authenticated native proof: set WAMN_NATIVE_B_AUTH_PG_URL to this proof's fresh disposable PostgreSQL 18 server / environment variable not found | [1973–1996](run-001/workspace.log#L1973) |
| wamn-host --test native_lifecycle_live | rebuilt_host_probes_signals_and_scheduler_recovery | missing_live_or_artifact_input | Error: set WAMN_HOST_LIVE_NATS_SERVER_BIN to the real NATS server binary | [2150–2152](run-001/workspace.log#L2150) |
| wamn-identity --test https_surface | identity_https_has_only_public_jwks_and_health | missing_live_or_artifact_input | assertion `left == right` failed: arm only an owned disposable cluster: this proof replaces platform schemas / left: Err(NotPresent) / right: Ok("1") | [2181–2188](run-001/workspace.log#L2181) |
| wamn-identity --test pat_issuance | operator_pat_issuance_over_https | missing_live_or_artifact_input | arm only an owned disposable cluster | [2201–2206](run-001/workspace.log#L2201) |
| wamn-identity --test session_exchange | session_exchange_uses_fresh_scoped_authority_without_session_state | missing_live_or_artifact_input | arm only an owned disposable cluster | [2218–2223](run-001/workspace.log#L2218) |
| wamn-platform-identity --test session_keys_live | session_key_lifecycle_on_postgres | missing_live_or_artifact_input | assertion `left == right` failed: arm only an owned disposable database: this test replaces platform schemas / left: Err(NotPresent) / right: Ok("1") | [2286–2292](run-001/workspace.log#L2286) |
| wamn-platform-identity --test session_keys_live | signer_backend_loss_before_commit_does_not_issue_token | missing_live_or_artifact_input | assertion `left == right` failed: arm only an owned disposable database: this test replaces platform schemas / left: Err(NotPresent) / right: Ok("1") | [2293–2299](run-001/workspace.log#L2293) |
| wamn-proof-conformance --lib | version_identity::wamn_wit_packages_stay_at_mvp_version | source_scan_misparses_retained_wit_evidence | The scan treats the trailing { in eight retained WIT package declarations as part of version 0.1.0 and reports version drift. | [2425–2436](run-001/workspace.log#L2425) |
| wamn-proof-conformance --test chart_seam_governance | receiving_pat_overlay_renders_a_complete_scoped_host | unavailable_kubernetes_discovery | kubectl cannot discover Kubernetes API types at localhost:8080 because KUBECONFIG=/dev/null and the connection is refused. | [2465–2471](run-001/workspace.log#L2465) |
| wamn-proof-conformance --test guest_workspace_closure | one_commit_built_in_two_checkouts_yields_identical_guest_digests | missing_live_or_artifact_input | WAMN_DIGEST_REPRO_A must name the first checkout's artifacts | [2557–2561](run-001/workspace.log#L2557) |
| wamn-proof-conformance --test guest_workspace_closure | one_commit_built_under_two_profiles_yields_identical_guest_digests | missing_live_or_artifact_input | WAMN_DIGEST_PROFILE_APP_PLAN must name the app artifact plan | [2562–2565](run-001/workspace.log#L2562) |
| wamn-proof-integration --lib | claim_law_live::a_changed_request_under_a_live_key_refuses_with_idempotency_conflict | missing_live_or_artifact_input | Error: WAMN_CLAIM_LAW_PG_URL names a fresh disposable PostgreSQL 18 database / Caused by: / environment variable not found | [2743–2747](run-001/workspace.log#L2743) |
| wamn-proof-integration --lib | claim_law_live::a_claim_that_updates_on_conflict_fails_both_emitted_cases | missing_live_or_artifact_input | Error: WAMN_CLAIM_LAW_PG_URL names a fresh disposable PostgreSQL 18 database / Caused by: / environment variable not found | [2748–2752](run-001/workspace.log#L2748) |
| wamn-proof-integration --lib | claim_law_live::a_replay_returns_the_immutable_original_result_and_writes_nothing | missing_live_or_artifact_input | Error: WAMN_CLAIM_LAW_PG_URL names a fresh disposable PostgreSQL 18 database / Caused by: / environment variable not found | [2753–2757](run-001/workspace.log#L2753) |
| wamn-proof-integration --lib | hot_route_trace::tests::an_incoming_traceparent_reaches_the_outbound_socket_under_the_same_trace | missing_live_or_artifact_input | set WAMN_HOTROUTE_PG_URL to a throwaway db | [2763–2767](run-001/workspace.log#L2763) |
| wamn-proof-integration --lib | provisionbench::tests::legacy_converges_database_authority_on_postgres | missing_live_or_artifact_input | WAMN_PG_ADMIN_URL must name a disposable PostgreSQL 18 instance: NotPresent | [2775–2778](run-001/workspace.log#L2775) |
| wamn-proof-integration --lib | provisionbench::tests::project_env_replay_uses_the_stored_instance_suffix | missing_live_or_artifact_input | WAMN_PG_ADMIN_URL must name a disposable PostgreSQL 18 instance: NotPresent | [2779–2782](run-001/workspace.log#L2779) |
| wamn-proof-integration --lib | receiving_data_access::tests::enum_and_optimistic_update_outcomes_hold_on_postgres_18 | missing_live_or_artifact_input | Error: WAMN_RECEIVING_PG_URL must name a fresh disposable PostgreSQL 18 database / Caused by: / environment variable not found | [2783–2787](run-001/workspace.log#L2783) |
| wamn-proof-integration --lib | receiving_data_access::tests::generated_update_ignores_ungranted_additive_columns | missing_live_or_artifact_input | Error: WAMN_RECEIVING_PG_URL must name a fresh disposable PostgreSQL 18 database / Caused by: / environment variable not found | [2788–2792](run-001/workspace.log#L2788) |
| wamn-proof-integration --lib | route_authentication_live::fresh_only::execution_tests::counter_uses_login_tenant_without_a_guest_guc | missing_live_or_artifact_input | Error: WAMN_TENANT_KEY_PG_URL must arm this disposable proof / Caused by: / environment variable not found | [2798–2802](run-001/workspace.log#L2798) |
| wamn-proof-integration --lib | route_authentication_live::overlay_compatibility::installed_contract_observer_preserves_acls_and_refuses_changed_requirements | missing_live_or_artifact_input | Error: environment variable not found | [2806–2807](run-001/workspace.log#L2806) |
| wamn-proof-integration --lib | route_authentication_live::postcommit::production_materializer_preserves_replay_and_progress | missing_live_or_artifact_input | Error: set WAMN_JOURNEY_DOCUMENT for the disposable Receiving route journey | [2809–2810](run-001/workspace.log#L2809) |
| wamn-proof-integration --lib | route_authentication_live::product_dev_command_owns_the_clean_twelve_stage_receipt_and_cleanup | missing_live_or_artifact_input | Error: set WAMN_ROUTE_PG18_URL for the disposable Receiving route journey | [2811–2812](run-001/workspace.log#L2811) |
| wamn-proof-integration --lib | route_authentication_live::production_materializer_consumes_the_causal_receipt_exactly_once | missing_live_or_artifact_input | Error: set WAMN_JOURNEY_DOCUMENT for the disposable Receiving route journey | [2813–2814](run-001/workspace.log#L2813) |
| wamn-proof-integration --lib | route_authentication_live::production_nested_fresh_only_requires_pat_and_observes_revocation | missing_live_or_artifact_input | Error: set WAMN_JOURNEY_DOCUMENT for the disposable Receiving route journey | [2815–2816](run-001/workspace.log#L2815) |
| wamn-proof-integration --lib | route_authentication_live::production_nested_session_call_preserves_original_caller | missing_live_or_artifact_input | Error: set WAMN_JOURNEY_DOCUMENT for the disposable Receiving route journey | [2817–2818](run-001/workspace.log#L2817) |
| wamn-proof-integration --lib | route_authentication_live::production_receiving_session_host_fixture | missing_live_or_artifact_input | Error: set WAMN_JOURNEY_DOCUMENT for the disposable Receiving route journey | [2819–2820](run-001/workspace.log#L2819) |
| wamn-proof-integration --lib | route_authentication_live::production_route_caller_authentication_and_operation_authorization | missing_live_or_artifact_input | WAMN_ROUTE_AUTH_PG18_URL must name a fresh disposable PostgreSQL 18 server: NotPresent | [2821–2824](run-001/workspace.log#L2821) |
| wamn-proof-integration --lib | route_authentication_live::production_session_client_login_and_fresh_selection | missing_live_or_artifact_input | Error: set WAMN_JOURNEY_DOCUMENT for the disposable Receiving route journey | [2825–2826](run-001/workspace.log#L2825) |
| wamn-proof-integration --lib | route_authentication_live::production_two_package_fresh_only_fixture_serves_all_thirteen_pat_routes | missing_live_or_artifact_input | Error: set WAMN_JOURNEY_DOCUMENT for the disposable Receiving route journey | [2827–2828](run-001/workspace.log#L2827) |
| wamn-proof-integration --lib | route_authentication_live::production_two_package_release_serves_all_thirteen_pat_routes | missing_live_or_artifact_input | Error: set WAMN_JOURNEY_DOCUMENT for the disposable Receiving route journey | [2829–2830](run-001/workspace.log#L2829) |
| wamn-proof-integration --lib | router_tap_live::tests::the_released_router_bridge_emits_accepted_and_settled_previews | missing_live_or_artifact_input | Error: set WAMN_ROUTER_TAP_NATS_URL for the disposable router-tap proof | [2832–2833](run-001/workspace.log#L2832) |
| wamn-proof-integration --lib | throughput_bench_live::tests::every_layer_ran_the_whole_sweep_and_its_knee_is_recorded | missing_live_or_artifact_input | WAMN_THROUGHPUT_EVIDENCE_DIR must point at a --throughput journey's throughput/ directory | [2843–2846](run-001/workspace.log#L2843) |
| wamn-proof-integration --lib | trusted_http_route::tests::nested_http_authorizes_child_and_preserves_original_caller | missing_live_or_artifact_input | Error: set WAMN_HTTP_REUSE_ALLOW_SCHEMA_RESET=1 for both disposable databases | [2847–2848](run-001/workspace.log#L2847) |
| wamn-proof-integration --lib | trusted_http_route::tests::real_http_guest_reuses_connections_without_reusing_authority | missing_live_or_artifact_input | Error: set WAMN_HTTP_REUSE_ALLOW_SCHEMA_RESET=1 for the disposable database | [2849–2850](run-001/workspace.log#L2849) |
| wamn-proof-integration --lib | virtualized_std_guest::tests::virtualized_artifacts_have_exact_imports_and_receiving_exports | missing_live_or_artifact_input | Error: set WAMN_STD_VIRTUALIZATION_COMPONENT_WASM for the virtualized std guest proof | [2851–2852](run-001/workspace.log#L2851) |
| wamn-proof-integration --lib | virtualized_std_guest::tests::virtualized_std_guest_hides_the_sentinel_and_maps_a_panic_to_a_typed_refusal | missing_live_or_artifact_input | Error: set WAMN_STD_VIRTUALIZATION_SENTINEL for the virtualized std guest proof | [2853–2854](run-001/workspace.log#L2853) |
| wamn-proof-integration --lib | wms_runtime_live::committed_move_survives_label_store_failure | missing_live_or_artifact_input | Error: set WAMN_JOURNEY_DOCUMENT for the disposable Receiving route journey | [2859–2860](run-001/workspace.log#L2859) |
| wamn-proof-integration --lib | wms_runtime_live::contention_and_replay_through_the_composed_route | missing_live_or_artifact_input | Error: set WAMN_JOURNEY_DOCUMENT for the disposable Receiving route journey | [2861–2862](run-001/workspace.log#L2861) |
| wamn-proof-integration --lib | wms_runtime_live::the_remaining_operations_serve_their_released_routes | missing_live_or_artifact_input | Error: set WAMN_JOURNEY_DOCUMENT for the disposable Receiving route journey | [2866–2867](run-001/workspace.log#L2866) |
| wamn-proof-integration --test receiving_command_histories_live | production_receiving_command_histories | missing_live_or_artifact_input | Error: WAMN_RECEIVING_CORRECTNESS_DOCUMENT must name the private fixture document | [2937–2938](run-001/workspace.log#L2937) |
| wamn-proof-integration --test startup_burst_live | production_http_start_burst_keeps_native_host_progress | missing_live_or_artifact_input | Error: WAMN_STARTUP_BURST_INPUT must name the runner-owned fixture | [2952–2954](run-001/workspace.log#L2952) |
| wamn-proof-system --test deploy_platform_inventory | every_mounted_secret_is_declared_here_or_named_a_prerequisite | deployment_prerequisite_declaration_mismatch | deploy/platform mounts Secrets it neither declares nor names as a prerequisite: [("wamn-object-store-credentials-acme--wms--dev", {"values-host-wms-pat.yaml"})] | [2977–2981](run-001/workspace.log#L2977) |
| wamn-run-state --test admission_live | surviving_authority_matrix_live | missing_live_or_artifact_input | set WAMN_RUN_STORE_PG_URL to the throwaway superuser database: NotPresent | [3208–3213](run-001/workspace.log#L3208) |
| wamn-run-state --test effect_writer_live | native_effect_writer_live | missing_live_or_artifact_input | set WAMN_RUN_STORE_PG_URL to a throwaway PostgreSQL database: NotPresent | [3225–3230](run-001/workspace.log#L3225) |
| wamn-run-state --test run_state_live | run_state_live | missing_live_or_artifact_input | set WAMN_RUN_STORE_PG_URL to the throwaway superuser database: NotPresent | [3265–3270](run-001/workspace.log#L3265) |
| wamn-runtime --lib | plugins::wamn_jetstream::tests::live_generic_guest_cannot_write_the_provisioned_tap_stream | missing_live_or_artifact_input | set WAMN_ROUTER_TAP_NATS_URL to the disposable provisioned NATS: NotPresent | [3502–3506](run-001/workspace.log#L3502) |
| wamn-runtime --lib | plugins::wamn_postgres::claims::tests::live_size_one_guest_and_platform_pools_isolate_sessions_under_interleaving | missing_live_or_artifact_input | set WAMN_POOL_LIFECYCLE_PG_URL to a disposable PostgreSQL database: NotPresent | [3555–3558](run-001/workspace.log#L3555) |
| wamn-runtime --lib | plugins::wamn_postgres::types::tests::live_a_timestamptz_read_back_spells_exactly_what_the_canonicalizer_spells | missing_live_or_artifact_input | set WAMN_CARRIER_SPELLING_PG_URL to a disposable PostgreSQL 18 database | [3624–3627](run-001/workspace.log#L3624) |
| wamn-runtime --lib | plugins::wamn_postgres::types::tests::live_every_carrier_that_passes_a_typed_value_as_text_matches_its_canonicalizer | missing_live_or_artifact_input | set WAMN_CARRIER_SPELLING_PG_URL to a disposable PostgreSQL 18 database | [3628–3631](run-001/workspace.log#L3628) |
| wamn-runtime --test executor_platform_surface_live | executor_platform_surface_live | missing_live_or_artifact_input | Error: set WAMN_EXEC_PLATFORM_PG_URL to a disposable superuser PostgreSQL url / Caused by: / environment variable not found | [3713–3718](run-001/workspace.log#L3713) |
| wamn-runtime --test production_claim_durable_live | production_claim_durable_live | missing_live_or_artifact_input | Error: set WAMN_DURABLE_TIER_PG_URL to a disposable PostgreSQL database (a DIFFERENT one from WAMN_PRODUCTION_CLAIM_PG_URL: both suites install the same schema and drop it on teardown) / Caused by: / environment variable not found | [3806–3811](run-001/workspace.log#L3806) |
| wamn-runtime --test production_claim_live | production_claim_live | missing_live_or_artifact_input | Error: set WAMN_PRODUCTION_CLAIM_PG_URL to a disposable PostgreSQL database / Caused by: / environment variable not found | [3823–3827](run-001/workspace.log#L3823) |
| wamn-runtime --test release_manifest_source | a_published_release_pulls_back_byte_exact_and_welds_the_release_it_names | missing_live_or_artifact_input | this live leg requires WAMN_RELEASE_MANIFEST_ARTIFACT_BASE: the explicit <registry>/<repository> the release was pushed to | [3854–3858](run-001/workspace.log#L3854) |
| wamn-runtime --test session_route_authentication | sessions_use_one_fresh_scoped_permission_union_and_preserve_the_signed_identity | missing_live_or_artifact_input | Error: set WAMN_SESSION_ROUTE_PG18_URL to this proof's fresh disposable PG18 server / Caused by: / environment variable not found | [3894–3899](run-001/workspace.log#L3894) |
| wamn-runtime --test sqlx_transaction_live | sqlx_command_commits_rolls_back_and_obeys_current_user_rls | missing_live_or_artifact_input | set WAMN_SQLX_TRANSACTION_PG_URL to a fresh PostgreSQL 18 superuser URL | [3924–3929](run-001/workspace.log#L3924) |
| wamn-runtime --test wiring_doorbell_live | a_pointer_flip_makes_the_cache_serve_the_new_active_version | missing_live_or_artifact_input | set WAMN_CATALOG_PG_URL to the throwaway superuser database: NotPresent | [3941–3946](run-001/workspace.log#L3941) |
| wamn-scenario-worker --test management_live | management_surface_authenticates_and_attributes_authoring_commands | missing_live_or_artifact_input | set WAMN_PLATFORM_IDENTITY_PG_URL to a disposable PostgreSQL superuser URL: NotPresent | [4023–4027](run-001/workspace.log#L4023) |
| wamn-schema-introspection --test postgres_live | an_exclusion_constraint_is_modelled_rather_than_refused | missing_live_or_artifact_input | WAMN_SCHEMA_INTROSPECTION_PG_URL must name a disposable PostgreSQL 18 server: NotPresent | [4447–4451](run-001/workspace.log#L4447) |
| wamn-schema-introspection --test postgres_live | receiving_migration_round_trips_and_refuses_unsupported_server_objects | missing_live_or_artifact_input | WAMN_SCHEMA_INTROSPECTION_PG_URL must name a disposable PostgreSQL 18 server: NotPresent | [4452–4456](run-001/workspace.log#L4452) |

| Explicit skip target | Test | Actual skip message | Raw log line |
| --- | --- | --- | ---: |
| unittests src/lib.rs | tests::pipelined_publish_lands_in_order_and_dedupes_live | WAMN_E1_NATS_URL unset — skipping E1 live JetStream gate | [556](run-001/workspace.log#L556) |
| tests/event_reader_live.rs | reader_streams_one_project_env_to_the_evt_stream | WAMN_READER_PG_URL unset — skipping the event-reader live gate | [583](run-001/workspace.log#L583) |
| tests/cdc.rs | cdc_role_reads_only_the_classification_maps_and_still_decodes_tenant_tables | skipping cdc_role_reads_only_the_entity_map_and_still_decodes_tenant_tables (set WAMN_CDC_PG_URL to run) | [1002](run-001/workspace.log#L1002) |
| tests/cdc.rs | cdc_substrate_applies_and_is_idempotent_on_postgres | skipping cdc_substrate_applies_and_is_idempotent_on_postgres (set WAMN_CDC_PG_URL to run) | [1004](run-001/workspace.log#L1004) |
| tests/control_portable_store.rs | control_author_is_tenant_bound_and_exactly_scoped_on_postgres | skipping control_author_is_tenant_bound_and_exactly_scoped_on_postgres (set WAMN_CONTROL_PORTABLE_PG_URL) | [1012](run-001/workspace.log#L1012) |
| tests/control_portable_store.rs | control_portable_store_enforces_the_current_record_on_postgres | skipping control_portable_store_enforces_the_current_record_on_postgres (set WAMN_CONTROL_PORTABLE_PG_URL) | [1014](run-001/workspace.log#L1014) |
| tests/control_portable_store.rs | deployment_attestation_rust_binding_holds_on_postgres | skipping deployment_attestation_rust_binding_holds_on_postgres (set WAMN_CONTROL_PORTABLE_PG_URL) | [1021](run-001/workspace.log#L1021) |
| tests/control_storage.rs | system_schema_applies_and_enforces_invariants_on_postgres | skipping system_schema_applies_and_enforces_invariants_on_postgres (set WAMN_REGISTRY_PG_URL to run) | [1042](run-001/workspace.log#L1042) |
| tests/database_owner.rs | project_env_database_ownership_and_connect_are_scoped | skipping project_env_database_ownership_and_connect_are_scoped (set WAMN_PROVISION_PG_URL to run) | [1053](run-001/workspace.log#L1053) |
| tests/deploy_sql_authority.rs | the_platform_arm_admits_every_platform_family_from_the_server | skipping the_platform_arm_admits_every_platform_family_from_the_server (set WAMN_TENANT_FLOOR_PG_URL to run) | [1061](run-001/workspace.log#L1061) |
| tests/deploy_sql_authority.rs | the_swept_floor_admits_only_the_connected_guest_on_postgres | skipping the_swept_floor_admits_only_the_connected_guest_on_postgres (set WAMN_TENANT_FLOOR_PG_URL to run) | [1064](run-001/workspace.log#L1064) |
| tests/deploy_sql_authority.rs | the_two_scenario_author_emitters_agree_at_zero_memberships | skipping the_two_scenario_author_emitters_agree_at_zero_memberships (set WAMN_TENANT_FLOOR_PG_URL to run) | [1066](run-001/workspace.log#L1066) |
| tests/dump.rs | dump_round_trips_a_seeded_database | skipping dump_round_trips_a_seeded_database (set WAMN_DUMP_PG_URL to run) | [1074](run-001/workspace.log#L1074) |
| tests/family_denial_matrix.rs | a_platform_family_without_tenant_context_reads_exactly_what_its_grants_say | skipping a_platform_family_without_tenant_context_reads_exactly_what_its_grants_say (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1082](run-001/workspace.log#L1082) |
| tests/family_denial_matrix.rs | every_governed_relation_carries_the_tenant_key_expression_index | skipping every_governed_relation_carries_the_tenant_key_expression_index (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1084](run-001/workspace.log#L1084) |
| tests/family_denial_matrix.rs | the_authority_derivations_match_their_pinned_definition_digest | skipping the_authority_derivations_match_their_pinned_definition_digest (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1087](run-001/workspace.log#L1087) |
| tests/family_denial_matrix.rs | the_dispatch_reader_family_is_refused_the_other_families_operations | skipping the denial matrix row for DispatchReader (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1089](run-001/workspace.log#L1089) |
| tests/family_denial_matrix.rs | the_effect_writer_arm_reaches_exactly_its_four_run_plane_ledgers | skipping the_effect_writer_arm_reaches_exactly_its_four_run_plane_ledgers (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1091](run-001/workspace.log#L1091) |
| tests/family_denial_matrix.rs | the_effect_writer_family_is_refused_the_other_families_operations | skipping the denial matrix row for EffectWriter (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1093](run-001/workspace.log#L1093) |
| tests/family_denial_matrix.rs | the_event_materializer_family_is_refused_the_other_families_operations | skipping the denial matrix row for EventMaterializer (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1095](run-001/workspace.log#L1095) |
| tests/family_denial_matrix.rs | the_executor_platform_family_is_refused_the_other_families_operations | skipping the denial matrix row for ExecutorPlatform (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1097](run-001/workspace.log#L1097) |
| tests/family_denial_matrix.rs | the_guest_family_reads_its_own_tenant_and_only_its_own | skipping the_guest_family_reads_its_own_tenant_and_only_its_own (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1099](run-001/workspace.log#L1099) |
| tests/family_denial_matrix.rs | the_guest_sql_family_is_refused_the_other_families_operations | skipping the denial matrix row for App (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1101](run-001/workspace.log#L1101) |
| tests/family_denial_matrix.rs | the_http_admitter_family_is_refused_the_other_families_operations | skipping the denial matrix row for HttpAdmitter (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1103](run-001/workspace.log#L1103) |
| tests/family_denial_matrix.rs | the_management_admitter_family_is_refused_the_other_families_operations | skipping the denial matrix row for ManagementAdmitter (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1105](run-001/workspace.log#L1105) |
| tests/family_denial_matrix.rs | the_platform_group_members_are_exactly_the_derived_families | skipping the_platform_group_members_are_exactly_the_derived_families (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1107](run-001/workspace.log#L1107) |
| tests/family_denial_matrix.rs | the_retention_family_is_refused_the_other_families_operations | skipping the denial matrix row for Retention (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1109](run-001/workspace.log#L1109) |
| tests/family_denial_matrix.rs | the_run_plane_guards_are_still_public_execute | skipping the_run_plane_guards_are_still_public_execute (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1111](run-001/workspace.log#L1111) |
| tests/family_denial_matrix.rs | the_scenario_author_has_no_platform_membership_or_project_reads | skipping the_scenario_author_has_no_platform_membership_or_project_reads (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1113](run-001/workspace.log#L1113) |
| tests/family_denial_matrix.rs | the_service_reader_family_is_refused_the_other_families_operations | skipping the denial matrix row for ServiceReader (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1115](run-001/workspace.log#L1115) |
| tests/family_denial_matrix.rs | the_session_role_reader_is_refused_the_other_families_operations | skipping the denial matrix row for SessionRoleReader (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1117](run-001/workspace.log#L1117) |
| tests/family_denial_matrix.rs | the_tenant_scoped_platform_member_reads_nothing_outside_its_grants | skipping the_tenant_scoped_platform_member_reads_nothing_outside_its_grants (set WAMN_DENIAL_MATRIX_PG_URL to run) | [1119](run-001/workspace.log#L1119) |
| tests/family_surface_grants.rs | the_event_materializer_role_holds_exactly_its_two_catalog_reads | skipping the_event_materializer_role_holds_exactly_its_two_catalog_reads (set WAMN_FAMILY_SURFACE_PG_URL to run) | [1127](run-001/workspace.log#L1127) |
| tests/family_surface_grants.rs | the_executor_platform_role_holds_exactly_its_measured_claim_surface | skipping the_executor_platform_role_holds_exactly_its_measured_claim_surface (set WAMN_FAMILY_SURFACE_PG_URL to run) | [1129](run-001/workspace.log#L1129) |
| tests/family_surface_grants.rs | the_http_admitter_role_adds_exactly_the_fresh_permission_reads | skipping the_http_admitter_role_adds_exactly_the_fresh_permission_reads (set WAMN_FAMILY_SURFACE_PG_URL to run) | [1131](run-001/workspace.log#L1131) |
| tests/operation_grants.rs | route_caller_grants_are_exact_residue_free_and_convergent_live | skipping route_caller_grants_are_exact_residue_free_and_convergent_live (set WAMN_OPERATION_GRANTS_PG18_URL to run) | [1156](run-001/workspace.log#L1156) |
| tests/ops_storage.rs | ops_schema_applies_idempotently_after_core_on_postgres | skipping ops_schema_applies_idempotently_after_core_on_postgres (set WAMN_REGISTRY_PG_URL to run) | [1166](run-001/workspace.log#L1166) |
| tests/provision.rs | provisioning_builders_apply_on_postgres | skipping provisioning_builders_apply_on_postgres (set WAMN_PROVISION_PG_URL to run) | [1176](run-001/workspace.log#L1176) |
| tests/provision.rs | the_platform_extensions_make_an_exclusion_constraint_reachable | skipping the_platform_extensions_make_an_exclusion_constraint_reachable (set WAMN_PROVISION_PG_URL to run) | [1178](run-001/workspace.log#L1178) |
| tests/restore.rs | restore_round_trips_and_clean_replaces_in_place | skipping restore gate (set WAMN_RESTORE_PG_URL to run) | [1186](run-001/workspace.log#L1186) |
| tests/system_reader_grants.rs | the_identity_reader_can_never_write_identity_and_neither_reader_reaches_the_other | skipping the_identity_reader_can_never_write_identity_and_neither_reader_reaches_the_other (set WAMN_REGISTRY_PG_URL to run) | [1225](run-001/workspace.log#L1225) |
| tests/system_reader_grants.rs | the_registry_reader_holds_one_select_and_is_refused_everywhere_else | skipping the_registry_reader_holds_one_select_and_is_refused_everywhere_else (set WAMN_REGISTRY_PG_URL to run) | [1227](run-001/workspace.log#L1227) |
| tests/tenant_key_live.rs | a_second_apply_leaves_the_definition_identical | skipping a_second_apply_leaves_the_definition_identical (set WAMN_TENANT_KEY_PG_URL to run) | [1235](run-001/workspace.log#L1235) |
| tests/tenant_key_live.rs | the_bootstrap_and_literal_renderings_install_the_same_object | skipping the_bootstrap_and_literal_renderings_install_the_same_object (set WAMN_TENANT_KEY_PG_URL to run) | [1237](run-001/workspace.log#L1237) |
| tests/tenant_key_live.rs | the_function_carries_the_flags_the_expression_index_requires | skipping the_function_carries_the_flags_the_expression_index_requires (set WAMN_TENANT_KEY_PG_URL to run) | [1239](run-001/workspace.log#L1239) |
| tests/tenant_key_live.rs | the_guest_may_execute_the_derivation_and_may_not_replace_it | skipping the_guest_may_execute_the_derivation_and_may_not_replace_it (set WAMN_TENANT_KEY_PG_URL to run) | [1241](run-001/workspace.log#L1241) |
| tests/tenant_key_live.rs | the_session_derivation_returns_the_key_of_the_connected_guest_login | skipping the_session_derivation_returns_the_key_of_the_connected_guest_login (set WAMN_TENANT_KEY_PG_URL to run) | [1243](run-001/workspace.log#L1243) |
| tests/tenant_key_live.rs | the_sql_tenant_key_equals_the_rust_tenant_key | skipping the_sql_tenant_key_equals_the_rust_tenant_key (set WAMN_TENANT_KEY_PG_URL to run) | [1245](run-001/workspace.log#L1245) |
| unittests src/lib.rs | push_component::tests::the_environment_instance_decides_which_run_owns_a_component_fact | skipping environment-instance projection proof; WAMN_CTL_PG_URL is unset | [1565](run-001/workspace.log#L1565) |
| unittests src/lib.rs | push_component::tests::verification_projection_replays_refuses_drift_and_leaves_publish_project_noop | skipping publication projection proof; WAMN_CTL_PG_URL is unset | [1569](run-001/workspace.log#L1569) |
| tests/apply_package_live.rs | exact_runner_commits_once_refuses_drift_and_rolls_back_a_failing_suffix | skipping apply-package live proof; WAMN_CTL_PG_URL is unset | [1637](run-001/workspace.log#L1637) |
| tests/dispatch_reader_provisioning_live.rs | dispatch_reader_provisioning_live | WAMN_CTL_PG_URL unset — skipping the wamn-0h0g.12.122 provisioning gate | [1692](run-001/workspace.log#L1692) |
| tests/guest_generation_live.rs | guest_generations_are_per_tenant_and_carry_the_predicate_key | skipping guest_generation_live (set WAMN_GUEST_GENERATION_PG18_URL to run) | [1717](run-001/workspace.log#L1717) |
| tests/package_data_access_live.rs | an_author_recovers_from_a_failed_version_bump_in_either_direction | skipping package_data_access_live; WAMN_CTL_PG_URL is unset | [1760](run-001/workspace.log#L1760) |
| tests/package_data_access_live.rs | installed_package_set_unions_a_real_app_generation_and_replays_noop | skipping package_data_access_live; WAMN_CTL_PG_URL is unset | [1762](run-001/workspace.log#L1762) |
| tests/package_data_access_live.rs | reconciliation_leaves_every_platform_schema_grant_on_the_app_role_standing | skipping package_data_access_live; WAMN_CTL_PG_URL is unset | [1764](run-001/workspace.log#L1764) |
| tests/provisioning_order_live.rs | a_refused_prepare_leaves_the_state_its_documentation_promises | skipping provisioning_order_live (set WAMN_PROVISIONING_ORDER_PG18_URL to run) | [1791](run-001/workspace.log#L1791) |
| tests/provisioning_order_live.rs | the_documented_provisioning_order_completes_end_to_end | skipping provisioning_order_live (set WAMN_PROVISIONING_ORDER_PG18_URL to run) | [1793](run-001/workspace.log#L1793) |
| tests/publish_release_live.rs | package_seal_and_attestation_winner_are_server_enforced | skipping publish-release live proof; WAMN_CTL_PG_URL is unset | [1801](run-001/workspace.log#L1801) |
| tests/replica_identity_live.rs | package_registration_union_flips_exact_tables_and_unreadable_state_refuses | skipping RI live proof; WAMN_CTL_PG_URL is unset | [1809](run-001/workspace.log#L1809) |
| tests/run_plane_live.rs | authoring_privileges_at_record_plan_no_repair_live | WAMN_CTL_PG_URL unset — skipping the authoring-privilege drift gate | [1817](run-001/workspace.log#L1817) |
| tests/run_plane_live.rs | child_run_cutover_live | WAMN_CTL_PG_URL unset — skipping the child-run cutover gate | [1819](run-001/workspace.log#L1819) |
| tests/run_plane_live.rs | dispatch_reader_read_surface_live | WAMN_CTL_PG_URL unset — skipping the dispatch-reader read-surface gate | [1821](run-001/workspace.log#L1821) |
| tests/run_plane_live.rs | environment_policy_row_security_live | WAMN_CTL_PG_URL unset — skipping the environment-policy RLS gate | [1828](run-001/workspace.log#L1828) |
| tests/run_plane_live.rs | reconcile_target_identity_guard_live | WAMN_CTL_PG_URL unset — skipping the run-plane target-identity gate | [1850](run-001/workspace.log#L1850) |
| tests/run_plane_live.rs | registry_durability_schema_ensure_live | WAMN_CTL_PG_URL unset — skipping the registry durability migration gate | [1852](run-001/workspace.log#L1852) |
| tests/run_plane_live.rs | rerun_lineage_cutover_live | WAMN_CTL_PG_URL unset — skipping the rerun-lineage cutover gate | [1854](run-001/workspace.log#L1854) |
| tests/run_plane_live.rs | retired_effect_disposition_cutover_live | WAMN_CTL_PG_URL unset — skipping retired disposition cutover gate | [1856](run-001/workspace.log#L1856) |
| tests/run_plane_live.rs | run_plane_reconcile_live | WAMN_CTL_PG_URL unset — skipping the wamn-1wdq run-plane gate | [1858](run-001/workspace.log#L1858) |
| tests/run_plane_live.rs | stored_suite_cutover_live | WAMN_CTL_PG_URL unset — skipping the stored-suite cutover gate | [1860](run-001/workspace.log#L1860) |
| tests/terminalize_effect_uncertain_live.rs | terminalize_effect_uncertain_is_atomic_exact_and_authority_closed_live | WAMN_OPERATOR_TERMINALIZE_PG18_URL unset — skipping operator terminalization gate | [1902](run-001/workspace.log#L1902) |
| tests/read_authority.rs | dispatcher_reads_the_queue_as_a_reader_that_cannot_write_it | skipping dispatcher_reads_the_queue_as_a_reader_that_cannot_write_it (set WAMN_PROVISION_PG_URL to run) | [1945](run-001/workspace.log#L1945) |
| tests/identity_live.rs | platform_identity_round_trip_on_postgres | skipping platform_identity_round_trip_on_postgres (set WAMN_PLATFORM_IDENTITY_PG_URL to run) | [2261](run-001/workspace.log#L2261) |
| tests/pat_live.rs | platform_pat_round_trip_on_postgres | skipping platform_pat_round_trip_on_postgres (set WAMN_PLATFORM_IDENTITY_PG_URL to run) | [2269](run-001/workspace.log#L2269) |
| tests/authority.rs | a_project_still_owns_its_own_configuration | skipping a_project_still_owns_its_own_configuration (set WAMN_SYSSCHEMA_PG_URL to run) | [2334](run-001/workspace.log#L2334) |
| tests/authority.rs | author_sql_cannot_write_the_relations_that_authorize_it | skipping author_sql_cannot_write_the_relations_that_authorize_it (set WAMN_SYSSCHEMA_PG_URL to run) | [2336](run-001/workspace.log#L2336) |
| tests/authority.rs | the_audited_party_may_append_to_its_trail_but_not_rewrite_it | skipping the_audited_party_may_append_to_its_trail_but_not_rewrite_it (set WAMN_SYSSCHEMA_PG_URL to run) | [2338](run-001/workspace.log#L2338) |
| tests/schema.rs | app_schema_applies_and_enforces_isolation_on_postgres | skipping app_schema_applies_and_enforces_isolation_and_claims_on_postgres (set WAMN_SYSSCHEMA_PG_URL to run) | [2346](run-001/workspace.log#L2346) |
| tests/store.rs | run_state_schema_applies_and_isolates_on_postgres | skipping run_state_schema_applies_and_isolates_on_postgres (set WAMN_RUN_STORE_PG_URL to run) | [3293](run-001/workspace.log#L3293) |
| unittests src/lib.rs | plugins::wamn_jetstream::tests::live_derived_publish_replay_converges_through_jetstream_dedup | skipping live_derived_publish_replay_converges_through_jetstream_dedup: WAMN_EVT_NATS_URL unset | [3500](run-001/workspace.log#L3500) |
| unittests src/lib.rs | plugins::wamn_jetstream::tests::live_publish_dedupe_bind_fetch_ack | skipping live_publish_dedupe_bind_fetch_ack: WAMN_EVT_NATS_URL unset | [3507](run-001/workspace.log#L3507) |
| unittests src/lib.rs | plugins::wamn_postgres::claims::tests::live_scs_off_server_fails_checkout_closed | WAMN_SCS_OFF_PG_URL unset — skipping the wamn-2jkm.65 R18 live negative (boot a postgres:18 with -c standard_conforming_strings=off; see docs/operations/build-and-test.md [R18-NEG]) | [3553](run-001/workspace.log#L3553) |
| tests/management_live.rs | empty_case_connection_component_without_release_or_binding_reports_zero_cases | skipping live connection Gate proof (set WAMN_PLATFORM_IDENTITY_PG_URL to run) | [4021](run-001/workspace.log#L4021) |
| tests/management_live.rs | management_surface_reconnects_after_the_verification_database_is_recreated | skipping management_surface_reconnects_after_the_verification_database_is_recreated (set WAMN_PLATFORM_IDENTITY_PG_URL to run) | [4028](run-001/workspace.log#L4028) |
| tests/management_live.rs | nonempty_case_connection_component_without_release_or_binding_refuses_effect_posture | skipping live connection Gate proof (set WAMN_PLATFORM_IDENTITY_PG_URL to run) | [4030](run-001/workspace.log#L4030) |

| Changed target | Baseline reported passes | Stage 1 reported passes | Removed case names | Added case names |
| --- | ---: | ---: | --- | --- |
| connection_wit_coherence | 3 | 3 | every_vendored_http_connection_contract_is_registered_and_byte_identical | every_vendored_http_connection_contract_is_byte_identical |
| node_wit_coherence | 2 | 1 | all_package_copies_are_registered | None |
| package_architecture | 9 | 0 | accepted_edge_pin_cannot_waive_a_forbidden_edge; core_and_contract_to_concrete_effect_are_rejected; guest_to_native_contamination_is_rejected; guest_to_native_transitive_contamination_is_rejected; manifest_classifies_both_workspaces_and_non_cargo_release_inputs; peer_composition_root_import_is_rejected; production_to_test_support_normal_and_build_edges_are_rejected; real_workspaces_satisfy_package_architecture; undocumented_native_deployable_is_rejected | None |
| postgres_wit_coherence | 2 | 1 | all_vendored_copies_are_registered | None |
| profile_selectors | 7 | 5 | component_build_distinguishes_absent_package_crates_from_inventory_drift; profile_contract_matches_locked_metadata; selector_tools_do_not_duplicate_canonical_package_inventory | component_build_requires_declared_app_crates_and_accepts_new_cargo_members |
| retained_root_outcomes | 1 | 0 | retained_root_map_exactly_covers_the_package_inventory | None |
| workspace_tiers | 9 | 0 | bare_cargo_commands_select_exact_defaults_and_workspace_remains_exhaustive; release_tier_requires_sr17_sr26_identity_join; workspace_tier_helper_dry_run_matches_manifest; workspace_tier_helper_full_plans_cover_both_workspaces; workspace_tier_helper_list_matches_manifest; workspace_tier_helper_refuses_invalid_and_empty_selections; workspace_tier_helper_runs_safely_outside_repository; workspace_tier_inventory_matches_live_cargo_metadata; workspace_tier_membership_matches_live_classification | None |
| wamn_proof_conformance | 66 | 58 | invocation::tests::all_vendored_node_abi_copies_are_registered_and_match_the_source; runtime_inventory::host_component_plugins_mutation_is_rejected; runtime_inventory::resolved_feature_and_deployed_workload_inventory_is_current; runtime_inventory::the_retained_workload_manifest_set_is_accepted; runtime_inventory::unrecorded_workload_manifest_mutation_is_rejected; runtime_inventory::vanished_workload_manifest_mutation_is_rejected; runtime_inventory::wasmtime_feature_policy_tests::control_a_removed_feature_is_refused; runtime_inventory::wasmtime_feature_policy_tests::control_an_absent_pin_is_refused; runtime_inventory::wasmtime_feature_policy_tests::control_an_unreviewed_feature_is_refused; runtime_inventory::wasmtime_feature_policy_tests::control_restoring_wasmtime_defaults_is_refused; runtime_inventory::wasmtime_feature_policy_tests::the_reviewed_pin_is_accepted | invocation::tests::all_vendored_node_abi_copies_match_the_source; runtime_inventory::deployed_workloads_preserve_runtime_policy; runtime_inventory::materializer_keeps_its_command_export |

The corrected extraction passes both focused commands at source `3406e858`. Receiving passes 14 tests, and WMS passes 12 tests. Their command ignores 17 cluster cases and takes 13.753 seconds. The platform command passes 3 schema cases and ignores 2 cases. It takes 13.734 seconds.

The 19 ignored cases are listed below and in `ignored-cases.json`. Eighteen require live fixtures. One regenerates the test input schema. No cluster was created, and no live result counts as a pass. The source bytes and modes stayed unchanged throughout the run.

The prior compile failures remain in `../app-owners-001/` and `../app-owners-002/`. The correction restores the existing constants without changing test bodies. The 38 rendering and helper tests already pass at `8b9025fa`. Their source did not change in the compile correction.

The large shell callers still run their current setup. They now select the app test crates and read structured WMS results. The drafted shorter entrypoints await the complete Rust runners. This record does not close stage 3 or replace its required workspace sweep.

| Command | Ignored case | Reason |
| --- | --- | --- |
| 1 | `receiving_data_access::tests::enum_and_optimistic_update_outcomes_hold_on_postgres_18` | requires a fresh disposable PostgreSQL 18 URL in WAMN_RECEIVING_PG_URL |
| 1 | `receiving_data_access::tests::generated_update_ignores_ungranted_additive_columns` | requires a fresh disposable PostgreSQL 18 URL in WAMN_RECEIVING_PG_URL |
| 1 | `route_authentication_live::dev::product_dev_command_owns_the_clean_twelve_stage_receipt_and_cleanup` | requires disposable PG18, NATS, authenticated OCI, and built wamn/host/flow-http binaries |
| 1 | `route_authentication_live::fresh_only::execution_tests::counter_uses_login_tenant_without_a_guest_guc` | requires WAMN_TENANT_KEY_PG_URL on a fresh disposable PostgreSQL 18 server |
| 1 | `route_authentication_live::materializer::production_materializer_consumes_the_causal_receipt_exactly_once` | requires the disposable Receiving journey after its production materializer settles |
| 1 | `route_authentication_live::overlay_compatibility::installed_contract_observer_preserves_acls_and_refuses_changed_requirements` | requires a fresh disposable PostgreSQL 18 server and retained evidence path |
| 1 | `route_authentication_live::postcommit::production_materializer_preserves_replay_and_progress` | requires the disposable Receiving journey after its causal materializer baseline |
| 1 | `route_authentication_live::routes::production_two_package_fresh_only_fixture_serves_all_thirteen_pat_routes` | requires the dedicated fresh-only disposable journey and copied package directory |
| 1 | `route_authentication_live::routes::production_two_package_release_serves_all_thirteen_pat_routes` | requires disposable PG18 and authenticated OCI plus built virtualized base, overlay, and flow-http artifacts |
| 1 | `route_authentication_live::sessions::production_nested_fresh_only_requires_pat_and_observes_revocation` | requires the fresh-only Receiving fixture, active identity issuer, public CA, and WAMN_SESSION_NESTED_HTTPS_ENDPOINT |
| 1 | `route_authentication_live::sessions::production_nested_session_call_preserves_original_caller` | requires the completed Receiving session fixture, active identity issuer, public CA, and WAMN_SESSION_NESTED_HTTPS_ENDPOINT |
| 1 | `route_authentication_live::sessions::production_receiving_session_host_fixture` | requires the completed disposable Receiving journey and WAMN_SESSION_HOST_FIXTURE_OUTPUT |
| 1 | `route_authentication_live::sessions::production_session_client_login_and_fresh_selection` | requires the fresh-only Receiving fixture, active identity issuer, public CA, and WAMN_SESSION_NESTED_HTTPS_ENDPOINT |
| 1 | `production_receiving_command_histories` | requires an owned disposable Receiving release and WAMN_RECEIVING_CORRECTNESS_DOCUMENT |
| 1 | `wms_runtime_live::committed_move_survives_label_store_failure` | requires the released WMS route after its disposable labels bucket is removed |
| 1 | `wms_runtime_live::contention_and_replay_through_the_composed_route` | requires the released WMS route on a disposable cluster, named by the journey document's runtime phase |
| 1 | `wms_runtime_live::the_remaining_operations_serve_their_released_routes` | requires the released WMS route on a disposable cluster, named by the journey document's runtime phase |
| 2 | `route_authentication_live::production_route_caller_authentication_and_operation_authorization` | requires a fresh disposable PG18 named by WAMN_ROUTE_AUTH_PG18_URL |
| 2 | `route_authentication_live::regenerate_checked_in_journey_schema` | schema regeneration command only |

# Architecture overhaul test report, November 2026

## 1. Summary

This report compares `7b9134a7b` with `60cf3bba8`, using the source counts in `raw-base.txt` and `raw-head.txt`.
Rust test attributes fell from 1,853 to 1,650, a reduction of 203 (`raw-base.txt`, `raw-head.txt`).
Test source fell by 9,654 lines, from 142,272 to 132,618, while other Rust source fell by 620 lines (the same inputs).
Ignored attributes fell from 345 to 340, while support source rose from 15,518 to 16,122 lines (the same inputs).
W37 removed statement admission and old SQL ownership, while T1 and T3 removed library-behavior checks, duplicate cases and test-only machinery (`rulings.md::W37`, `deleted-tests.md`, `t3a-audit.md` through `t3f-audit.md`).
T2 moved identifier refusals into parsed types, which reduced identifier string parameters from 571 to 228 and digest string declarations from 137 to zero (`raw-base.txt`, `raw-head.txt`).
W42, W45 and W48 replaced privilege rosters with observed refusals and derived database rules (`22f4f6f00`, `10a056424`, `e75b18cef`).
The counts describe source definitions, and this documentation task ran no tests or builds (`counts.md`, this report's preparation scope).

## 2. Counts

All draft names below refer to files under `/var/tmp/wamn-arch/report/`.
`rulings.md` means [the implementation rulings](../wamn-architecture-review-full/rulings.md).
The plan is [Part 4 of the review report](../wamn-architecture-review-full/review-report.md#part-4-order-of-work).
[CLAUDE.md](../../CLAUDE.md#test-rules) supplies test rules 1 through 5.
`counts.md`, `deleted-tests.md`, `changed-tests.md`, `moved-tests.md` and `product-fixes.md` stop at `140a36c2b`.
Their interim totals do not describe this report's head (`counts.md`).

| Measure | Before | After | Change |
| --- | ---: | ---: | ---: |
| Rust test attributes | 1,853 | 1,650 | -203 |
| Ignore attributes | 345 | 340 | -5 |
| Test source lines | 142,272 | 132,618 | -9,654 |
| Other Rust source lines | 160,815 | 160,195 | -620 |
| Test support lines | 15,518 | 16,122 | +604 |
| Identifier string parameters | 571 | 228 | -343 |
| Identifier String fields | 272 | 126 | -146 |
| String validators | 57 | 26 | -31 |
| Digest string declarations | 137 | 0 | -137 |

Source: `raw-base.txt`, `raw-head.txt` and the totals supplied with this lane brief.
Support lines form part of test lines, so the two line measures must not be added (`counts.md`).
The script counts tracked Rust attributes and source lines, without expanding macros or running tests (`counts.md`).
The TypeScript callback inventory in `counts.md` is separate from these Rust totals.

The following table maps source owners across directory and package renames.
It lists only owners whose Rust test count changed (`raw-base.txt`, `raw-head.txt`, `counts.md`).
Events combines its old crate and transport crate, because W43 merged those owners (`ce6844f93`).
Authority, HTTP and SQL map their renamed source owners directly, without counting the rename as a deletion (`ce6844f93`).

| Source owner before | Source owner after | Before | After | Change |
| --- | --- | ---: | ---: | ---: |
| `apps/wamn_receiving/tests` | `apps/wamn_receiving/tests` | 16 | 10 | -6 |
| `crates/platform/authentication` | `crates/authentication` | 36 | 16 | -20 |
| `crates/platform/capability-authority` | `crates/authority` | 16 | 18 | +2 |
| `crates/platform/blobstore` | `crates/blobstore` | 46 | 50 | +4 |
| `crates/broker` | `crates/broker` | 23 | 12 | -11 |
| `crates/catalog/model` | `crates/catalog` | 117 | 108 | -9 |
| `crates/schema/generator` | `crates/compiler` | 220 | 203 | -17 |
| `crates/platform/component-artifact` | `crates/component-artifact` | 22 | 8 | -14 |
| `crates/platform/component-composer` | `crates/component-composer` | 6 | 4 | -2 |
| `crates/platform/component-policy` | `crates/component-policy` | 16 | 14 | -2 |
| `crates/control/dev` | `crates/dev` | 22 | 17 | -5 |
| `crates/platform/digest` | `crates/digest` | 4 | 5 | +1 |
| `crates/platform/engine` | `crates/engine` | 109 | 101 | -8 |
| `crates/control/environment` | `crates/environment` | 122 | 104 | -18 |
| `crates/events/wire` | `crates/event-wire` | 7 | 6 | -1 |
| `crates/platform/events-capability` + `crates/events` | `crates/events` | 7 | 18 | +11 |
| `crates/execution/host` | `crates/execution-host` | 88 | 76 | -12 |
| `crates/platform/http-capability` | `crates/http` | 78 | 65 | -13 |
| `crates/identity/platform` | `crates/identity` | 59 | 46 | -13 |
| `crates/catalog/package-artifact` | `crates/package-artifact` | 24 | 23 | -1 |
| `crates/control/platform-install` | `crates/platform-install` | 48 | 45 | -3 |
| `apps/platform/data/postgres-statements` | `crates/postgres-statements` | 6 | 3 | -3 |
| `crates/control/provision` | `crates/provision` | 116 | 93 | -23 |
| `crates/control/publish` | `crates/publish` | 68 | 61 | -7 |
| `crates/control/registry` | `crates/registry` | 5 | 10 | +5 |
| `crates/execution/run-state` | `crates/run-state` | 4 | 2 | -2 |
| `crates/schema/introspection` | `crates/schema-introspection` | 15 | 14 | -1 |
| `crates/platform/postgres-capability` | `crates/sql` | 120 | 87 | -33 |
| `crates/execution/workflow` | `crates/workflow` | 68 | 71 | +3 |
| `apps/platform/execution/jsonata` | `guests/jsonata` | 8 | 7 | -1 |
| `services/ctl` | `services/ctl` | 34 | 32 | -2 |
| `services/identity` | `services/identity` | 21 | 19 | -2 |

The table does not count moved tests as removed coverage.
Broker tests moved to Events and Authority, while authentication and identity database tests moved to SQL (`moved-tests.md`).
Provision gained persistence and history cases before T3 reduced its duplicate definitions (`moved-tests.md`, `d39c50613`).
The seven T3 areas occupy six audit files because `t3a-audit.md` covers both Component Artifact and Dev.

## 3. Deleted tests

Each row names a removed source definition and its source owner.
The reason groups follow the owner's test rules and the recorded deletion evidence (`deleted-tests.md`, T3 audits).
Section 4 names cases absorbed into surviving tables, rather than counting them again here.
A removed assertion does not by itself establish that the underlying contract disappeared (`deleted-tests.md::Review limits`).

### Pinned roster

| Crate or source owner | Deleted test | Reason | Commit |
| --- | --- | --- | --- |
| compiler | `authored_feed_grouped_summary_records_both_link_directions` | Rule 2 removes a roster or an output-presence assertion. | `fd0b5059d` |
| compiler | `authored_feed_receipt_screen_records_its_join_path` | Rule 2 removes a roster or an output-presence assertion. | `fd0b5059d` |
| compiler | `enum_fields_come_from_the_check_and_an_authored_list_only_narrows` | Rule 2 removes a roster or an output-presence assertion. | `fd0b5059d` |
| environment | `integration_and_target_seeds_live_only_in_the_system_database` | The expected relation set comes from ownership rules. W42 and rule 2. | `4e856d583` |
| provision | `the_platform_arm_admits_every_platform_family_from_the_server` | W44 replaces role rosters with refusals and catalog rules. | `7d6c3e0c4` |

### Presence or exact-text check

| Crate or source owner | Deleted test | Reason | Commit |
| --- | --- | --- | --- |
| catalog | `the_configmap_name_is_a_dns_1123_subdomain` | The case pins output text, a constant or a document snapshot. Rule 2. | `deb502710` |
| catalog | `the_requirement_hash_bytes_are_pinned` | The case pins output text, a constant or a document snapshot. Rule 2. | `deb502710` |
| compiler | `an_operation_stem_reads_the_contract_name_out_of_an_identity` | W44 deletes the internal spelling-helper test. | `6e4b359fe` |
| compiler | `exposes authored descriptions through TypeScript symbols` | Rule 2 removes a roster or an output-presence assertion. | `fd0b5059d` |
| compiler | `retired_workflows_are_dropped` | Rule 2 removes a roster or an output-presence assertion. | `fd0b5059d` |
| component-policy | `denied_package_literal_is_pinned` | Rule 2 deletes the package-literal pin. | `772ac4c66` |
| engine | `release_label_is_the_digest_as_lowercase_unpadded_base32` | The case pins output text, a constant or a document snapshot. Rule 2. | `deb502710` |
| execution-host | `package_signed_read_admits_the_real_node_abi_and_manifest_owned_authority` | The case admits metadata without invoking the operation. Rule 2. | `9ff10cf42` |
| platform-install | `the_committed_cluster_compiles_to_the_runbook_values` | The case pins output text, a constant or a document snapshot. Rule 2. | `389bab3c6` |
| platform-install | `the_committed_kind_cluster_compiles_without_gcp_inputs` | The case pins output text, a constant or a document snapshot. Rule 2. | `389bab3c6` |
| provision | `a_domain_name_renders` | The case pins output text, a constant or a document snapshot. Rule 2. | `564c7493f` |
| provision | `administration_uses_the_database_boundary_while_catalog_keeps_project_keys` | The case pins output text, a constant or a document snapshot. Rule 2. | `d39c50613` |
| provision | `templates_reproduce_the_retired_tiers` | The case pins output text, a constant or a document snapshot. Rule 2. | `d39c50613` |
| provision | `the_audit_retention_role_comes_from_the_catalog_schema_alone_on_postgres` | W44 deletes the role-presence probe. | `7d6c3e0c4` |
| provision | `the_login_prefix_carries_no_regex_metacharacter` | The case pins output text, a constant or a document snapshot. Rule 2. | `d39c50613` |
| service identity | `an_invite_url_makes_each_mail_one_line_and_one_link` | The case pins output text, a constant or a document snapshot. Rule 2. | `5ca0f6c6a` |
| sql | `a_db_call_inside_a_node_walk_carries_its_run_and_node_identity` | The old full span-map comparison goes. The draft supplies no test-specific rationale beyond W37. | `d8274014b` |
| sql | `a_db_call_outside_a_node_walk_records_the_wiring_keys_empty` | The old full span-map comparison goes. The draft supplies no test-specific rationale beyond W37. | `d8274014b` |

### Test of a test helper

This group also records private helper tests without a product-interface observation (`t3c-audit.md`, `kclrt-audit.md`).

| Crate or source owner | Deleted test | Reason | Commit |
| --- | --- | --- | --- |
| component-artifact | `a_guest_reads_the_environment_its_context_carries` | T3 removes a probe-only guest test. Engine owns ambient-environment isolation. | `8b98b7ec4` |
| environment | `file_fixture_assembles_an_http_role_without_kind` | The case checks fixture, probe or private setup behavior. Rules 1 and 3. | `4e856d583` |
| environment | `file_fixture_declares_each_binding_mode` | The case checks fixture, probe or private setup behavior. Rules 1 and 3. | `4e856d583` |
| http | `retired_client_leases_outlive_cache_entries_and_body_cancellation` | T3 removes assertions on private cache internals. Rule 1. | `56d415835` |
| platform-install | `service_chart_resolution_prefers_own_then_default_then_fresh_supplied` | The case checks fixture, probe or private setup behavior. Rules 1 and 3. | `389bab3c6` |
| provision | `a_target_document_preserves_its_coordinates` | T3 removes the fixture round trip. Session target behavior remains covered. | `d39c50613` |
| provision | `current_database_connect_posture_is_exactly_targeted` | The case checks fixture, probe or private setup behavior. Rules 1 and 3. | `d39c50613` |
| receiving tests | `infrastructure_failure_cannot_replace_a_business_counterexample` | The remaining history model cases test their own model or shrinker. Rule 3. | `b75a8b140` |
| receiving tests | `refused_attempt_does_not_claim_the_key_or_change_business_state` | The remaining history model cases test their own model or shrinker. Rule 3. | `b75a8b140` |
| receiving tests | `replay_preserves_the_result_before_completion_and_later_updates` | The remaining history model cases test their own model or shrinker. Rule 3. | `b75a8b140` |
| receiving tests | `shrinking_preserves_the_original_business_property_and_outcome` | The remaining history model cases test their own model or shrinker. Rule 3. | `b75a8b140` |
| receiving tests | `stale_updates_and_replays_do_not_advance_the_revision` | The remaining history model cases test their own model or shrinker. Rule 3. | `b75a8b140` |

### Duplicate of a lower-tier test

| Crate or source owner | Deleted test | Reason | Commit |
| --- | --- | --- | --- |
| authentication | `age_rules_remain_separate_at_their_exact_boundaries` | T3 keeps the contract at its observing owner. Rule 1. | `c3fb7275d` |
| authentication | `exact_org_and_environment_matrix_uses_one_configured_issuer_cache` | T3 keeps the contract at its observing owner. Rule 1. | `c3fb7275d` |
| authentication | `two_local_verifiers_refuse_removed_key_by_deadline_with_and_without_jwks` | T3 keeps the contract at its observing owner. Rule 1. | `c3fb7275d` |
| authentication | `unknown_key_ids_across_clones_share_the_issuer_refresh_budget` | T3 keeps the contract at its observing owner. Rule 1. | `c3fb7275d` |
| authentication | `verification_near_the_key_deadline_admits_across_it` | T3 keeps the contract at its observing owner. Rule 1. | `c3fb7275d` |
| catalog | `a_hostname_cannot_reach_an_authored_route` | Derived serde behavior belongs to the library. Custom product refusals remain. | `0491e5f4e` |
| catalog | `declaration_round_trips_and_refuses_unknown_fields` | Derived serde behavior belongs to the library. Custom product refusals remain. | `0491e5f4e` |
| catalog | `removed_flow_and_plan_fields_are_refused` | Derived serde behavior belongs to the library. Custom product refusals remain. | `0491e5f4e` |
| compiler | `the_retired_participant_keys_are_refused` | Derived serde behavior belongs to the library. Custom product refusals remain. | `0491e5f4e` |
| component-artifact | `a_guest_never_sees_the_host_process_environment` | T3 keeps the contract at its observing owner. Rule 1. | `8b98b7ec4` |
| component-artifact | `a_mutable_or_ambient_base_refuses_before_any_transport` | T3 keeps the contract at its observing owner. Rule 1. | `8b98b7ec4` |
| component-artifact | `a_served_body_the_descriptor_undercounts_refuses_the_pull` | T3 keeps the contract at its observing owner. Rule 1. | `8b98b7ec4` |
| component-artifact | `pulled_bytes_load_the_release_naming_their_carrier` | T3 keeps the contract at its observing owner. Rule 1. | `8b98b7ec4` |
| component-artifact | `source_configuration_is_explicit_and_redacts_rejected_credentials` | T3 keeps the contract at its observing owner. Rule 1. | `8b98b7ec4` |
| environment | `prekind_environment_renders` | T3 keeps the contract at its observing owner. Rule 1. | `4e856d583` |
| environment | `prekind_receiving_pat_renders` | T3 keeps the contract at its observing owner. Rule 1. | `4e856d583` |
| environment | `prekind_wms_pat_renders` | T3 keeps the contract at its observing owner. Rule 1. | `4e856d583` |
| event-wire | `causation_carries_run_root_depth` | Derived serde behavior belongs to the library. Custom product refusals remain. | `0491e5f4e` |
| events | `reader_configuration_refuses_control_credentials_and_missing_capture_names` | Derived serde behavior belongs to the library. Custom product refusals remain. | `0491e5f4e` |
| execution-host | `exposure_transitions_retain_public_urls_and_repeat_origin_deletion` | T3 removes a mock duplicate of live file-publication coverage. Rules 1 and 3. | `34b3045a3` |
| execution-host | `four_file_operations_require_an_explicit_application_grant` | T3 keeps the contract at its observing owner. Rule 1. | `34b3045a3` |
| execution-host | `native_node_cancellation_revokes_invocation_authority` | T3 keeps the contract at its observing owner. Rule 1. | `34b3045a3` |
| http | `mixed_answers_pin_allowed_peer_and_rebinding_refuses` | T3 keeps the contract at its observing owner. Rule 1. | `56d415835` |
| provision | `cdc_names_encode_the_triple_in_both_domains` | T2 removes caller-side name derivation checks. Registry owns resource-coordinate separation. | `de6502e3d` |
| provision | `private_custom_operations_never_become_role_grants` | T3 keeps the contract at its observing owner. Rule 1. | `d39c50613` |
| provision | `project_env_names_encode_the_full_triple` | T2 removes caller-side name derivation checks. Registry owns resource-coordinate separation. | `de6502e3d` |
| provision | `the_environment_namespace_carries_the_minted_instance_suffix` | T2 removes caller-side name derivation checks. Registry owns resource-coordinate separation. | `de6502e3d` |
| provision | `the_function_carries_the_flags_the_expression_index_requires` | T3 keeps the contract at its observing owner. Rule 1. | `d39c50613` |
| provision | `the_history_read_reconstructs_a_row_at_retained_positions_on_postgres` | T3 removes a test-only history query. Record-history fold tests retain reconstruction. | `d39c50613` |
| provision | `the_project_key_is_the_workload_identity_digest_and_not_a_second_one` | T3 keeps the contract at its observing owner. Rule 1. | `d39c50613` |
| provision | `the_registry_and_provisioning_decide_a_project_id_alike` | T3 keeps the contract at its observing owner. Rule 1. | `d39c50613` |
| provision | `the_runtime_reachable_digest_is_the_same_one` | T3 keeps the contract at its observing owner. Rule 1. | `d39c50613` |
| provision | `the_sql_project_key_equals_the_rust_project_key` | T3 keeps the contract at its observing owner. Rule 1. | `d39c50613` |
| schema-introspection | `catalog_retains_view_columns_and_dependencies` | Derived serde behavior belongs to the library. Custom product refusals remain. | `0491e5f4e` |
| service identity | `pat_request_refuses_extra_duplicate_and_non_integer_fields` | Derived serde behavior belongs to the library. Custom product refusals remain. | `0491e5f4e` |

### Benchmark

| Crate or source owner | Deleted test | Reason | Commit |
| --- | --- | --- | --- |
| receiving tests | `open_receipt_lines_feed_measurement` | W47 removes this measurement from the acceptance tests. | `7f5ae5d93` |

### Removed product feature

| Crate or source owner | Deleted test | Reason | Commit |
| --- | --- | --- | --- |
| catalog | `each_member_of_one_operation_binds_only_its_own_statements` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| catalog | `generated_statement_facts_bind_to_exact_registered_export` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| catalog | `manifest_matched_private_operation_can_carry_sql_without_an_auth_token` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| catalog | `profile_reads_compiled_facts_and_expands_view_dependencies` | W40 removes statement reaches from release profiles. | `b81aed759` |
| catalog | `statement_authority_refuses_wrong_operation_path_and_shape` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| catalog | `statement_lookup_is_operation_targeted_and_revalidates_its_key` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| catalog | `the_imports_fingerprint_covers_the_statement_instances` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| compiler | `admits_platform_reads_and_application_sql` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| compiler | `nested_platform_packages_share_the_application_workspace` | S13-G deletes special lookup for nested platform applications. | `7682829c2` |
| compiler | `operation_connections_refuse_sql_and_host_transactions` | W37 deletes per-operation connection declarations and SQL exclusion. | `343e5fb07` |
| compiler | `refuses_counter_function_calls` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| compiler | `refuses_do_blocks_and_function_bodies` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| compiler | `refuses_dynamic_execute` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| compiler | `refuses_logical_messages` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| compiler | `refuses_platform_writes` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| compiler | `refuses_session_writes` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| compiler | `refuses_unicode_escapes` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| compiler | `refuses_unparsed_and_multiple_statements` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| compiler | `refuses_xml_query_functions` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| component-composer | `each_statements_importer_gets_its_own_outer_import_with_its_digest` | Removed with statement-import fixtures. The draft does not separately establish loss of resource-decoding coverage. | `d8274014b` |
| component-composer | `resource_bearing_sql_imports_decode_after_overlay_composition` | Removed with statement-import fixtures. The draft does not separately establish loss of resource-decoding coverage. | `d8274014b` |
| component-policy | `wasi_rows_match_the_family_and_authored_rows_stay_exact` | W37 deletes patch-version arithmetic. Exact project-policy refusals remain. | `0bcbaf87a` |
| engine | `a_member_cannot_receive_statements_from_an_unnamed_import` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| engine | `a_statement_import_cannot_reach_a_member_under_another_name` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| engine | `a_statement_import_must_reach_exactly_one_member` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| engine | `a_statement_import_must_reach_its_recorded_member` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| engine | `functions_and_instances_cannot_be_aliased_out_of_a_named_import` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| engine | `named_statement_imports_and_the_record_must_agree` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| engine | `shared_and_named_statement_imports_cannot_coexist` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| environment | `create_user_stores_the_email_folded_to_lower_case` | W37 removes this product path. | `0bcbaf87a` |
| environment | `one_email_address_names_one_principal` | W37 removes this product path. | `0bcbaf87a` |
| environment | `only_a_user_row_carries_an_email` | W37 removes this product path. | `0bcbaf87a` |
| execution-host | `claimed_requests_stop_at_the_invocation_deadline_or_cancellation` | The old private buffer or disabled lifetime target goes with its subject. | `34b3045a3` |
| execution-host | `delayed_grant_cannot_restore_revoked_admin` | W44 removes ProjectionPause. Projection reads current locked authority and accepts no saved grant. | `4ab80890d` |
| execution-host | `named_instances_without_released_sets_are_refused` | W37 removes statement admission or the transaction exclusion rule. | `284f681bd` |
| execution-host | `upload_body_exceeds_the_old_buffer_ceiling_without_collection` | The old private buffer or disabled lifetime target goes with its subject. | `34b3045a3` |
| guest jsonata | `only_an_array_fits_the_items_port` | W46/W47 replace the guest-only array restriction with declared schemas and runtime validation. | `508f0b954` |
| http | `connection_resolution_scopes_sql_and_external_authority` | W37 removes statement admission or the transaction exclusion rule. | `284f681bd` |
| identity | `the_application_advances_a_counter_only_through_bump_counter` | W37 deletes the host numbering path. | `9ff10cf42` |
| identity | `the_application_assigns_a_keyed_counter_only_through_assign_counter` | W37 deletes the host numbering path. | `9ff10cf42` |
| identity | `the_application_sets_a_counter_only_through_set_counter` | W37 deletes the host numbering path. | `9ff10cf42` |
| postgres-statements | `callback_error_does_not_commit_or_retry_serialization_failure` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| postgres-statements | `callback_success_commits_exactly_once` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| postgres-statements | `query_errors_keep_the_sqlstate_and_database_message` | Deleted with old wrappers. The draft gives no separate reason for the surviving typed QueryError assertion. | `d8274014b` |
| publish | `composed_schema_uses_base_dependencies_without_copied_models` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| publish | `composition_refuses_a_substituted_statement_owner` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| publish | `composition_refuses_a_wrapper_that_forwards_sql_to_the_wrong_leaf` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| publish | `declared_sql_that_calls_a_counter_function_is_refused_at_admission` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| publish | `package_evidence_binds_exact_sql_to_each_manifest_operation` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| publish | `release_projection_preserves_operation_targeted_statement_facts` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| publish | `unsafe_paths_are_typed_statement_refusals` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| service ctl | `invite_arguments_repeat_grants_and_split_memberships` | W37 removes this product path. | `0bcbaf87a` |
| sql | `a_counter_renders_padded_and_never_truncated` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `a_read_operation_refuses_a_writing_statement_and_a_command_runs_it` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| sql | `a_refusal_keeps_its_cause_until_the_statement_boundary` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `a_transactional_statement_without_an_executing_principal_is_refused` | W37 removes statement admission or the transaction exclusion rule. | `284f681bd` |
| sql | `activation_exposes_only_the_selected_operation_and_revoke_closes_it` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| sql | `an_explicit_transaction_statement_without_an_executing_principal_is_refused` | W37 removes statement admission or the transaction exclusion rule. | `284f681bd` |
| sql | `an_unverified_declared_statement_is_refused_before_a_stream_opens` | W37 removes statement admission or the transaction exclusion rule. | `284f681bd` |
| sql | `anonymous_caller_begins_an_operation_without_a_grant` | W38 requires an executing principal for every call. | `377ae7a62` |
| sql | `anonymous_read_authorization_needs_no_database_credentials` | W38 requires an executing principal for every call. | `377ae7a62` |
| sql | `bind_mismatch_reports_expected_and_observed_shapes` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| sql | `building_a_pool_requires_a_probeable_credential` | W37/W38 replace the old pool and identity-setting machinery. | `0cae817fb` |
| sql | `counters_take_name_order_after_the_counter_their_key_names` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `each_named_import_runs_only_its_member_set_while_the_operation_is_active` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| sql | `exact_scope_cleanup_preserves_a_scope_with_the_same_prefix` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| sql | `explicit_statement_run_restores_only_after_success` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| sql | `guest_and_platform_pool_caches_remain_distinct_under_interleaving` | W37/W38 replace the old pool and identity-setting machinery. | `0cae817fb` |
| sql | `live_a_call_outside_an_operation_transaction_is_refused` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_a_call_with_no_application_in_host_state_is_refused` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_a_caller_numbers_with_no_record_and_with_a_key_map` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_a_refused_key_moves_no_counter` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_a_rewritten_application_identity_reaches_no_other_sequence` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_a_rollback_gives_the_number_back` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_a_rolled_back_per_assignment_is_not_consumed` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_a_sequence_of_another_application_is_refused` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_a_week_pattern_renders_the_iso_year` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_an_unverified_declared_statement_is_refused_when_it_runs` | W37 removes statement admission or the transaction exclusion rule. | `284f681bd` |
| sql | `live_autocommit_settings_are_reused_only_on_their_connection` | W37/W38 replace the old pool and identity-setting machinery. | `0cae817fb` |
| sql | `live_begin_with_identity_sets_the_guest_set_without_a_project_env_key_identity` | W37/W38 replace the old pool and identity-setting machinery. | `0cae817fb` |
| sql | `live_bound_identity_are_injection_inert_and_txn_local` | W37/W38 replace the old pool and identity-setting machinery. | `0cae817fb` |
| sql | `live_connect_asserts_standard_conforming_strings` | W37/W38 replace the old pool and identity-setting machinery. | `0cae817fb` |
| sql | `live_numbers_are_gapless_and_consecutive_under_concurrent_creates` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_per_assigns_once_per_key_under_concurrency` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_reset_starts_at_one_per_key` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_scs_off_server_fails_checkout_closed` | W37/W38 replace the old pool and identity-setting machinery. | `0cae817fb` |
| sql | `live_set_counter_advances_resets_and_the_model_uq_catches_a_live_number` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_set_counter_serializes_with_a_concurrent_next_number` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_set_counter_takes_a_reset_key_and_refuses_a_per_counter` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_set_errors_keep_the_request_and_source` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `live_size_one_guest_and_platform_pools_isolate_sessions_under_interleaving` | W37/W38 replace the old pool and identity-setting machinery. | `0cae817fb` |
| sql | `live_stream_reads_once_and_scopes_identity` | W37 removes statement admission or the transaction exclusion rule. | `284f681bd` |
| sql | `live_the_date_uses_the_application_timezone_else_utc` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `refused_statement_authority_returns_unknown_statement` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| sql | `result_mismatch_reports_the_column_contract` | W37 removes admitted statements and their guest wrappers. | `d8274014b` |
| sql | `the_date_formats_name_every_date_token_and_date_key` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |
| sql | `the_keys_carry_every_value_the_pattern_names_as_text` | W37 deletes host numbering. Guest SQL owns numbering. | `68cc32663` |

### Replaced by a newtype parse test

| Crate or source owner | Deleted test | Reason | Commit |
| --- | --- | --- | --- |
| catalog | `a_manifest_digest_admits_exactly_the_run_plane_shape` | T2 moves the format refusal to the parsed identifier owner. | `deb502710` |
| catalog | `attachment_operation_is_explicit_and_canonical` | T2 moves the format refusal to the parsed identifier owner. | `564c7493f` |
| catalog | `every_manifest_hash_validates_during_deserialization` | T2 moves the format refusal to the parsed identifier owner. | `deb502710` |
| catalog | `package_id_wamn_is_reserved` | T2 moves the format refusal to the parsed identifier owner. | `da9f6111a` |
| catalog | `persisted_component_target_uses_the_package_coordinate_guard` | T2 moves the format refusal to the parsed identifier owner. | `da9f6111a` |
| catalog | `persisted_coordinates_preserve_constructor_invariants` | T2 moves the format refusal to the parsed identifier owner. | `e2c9c97c3` |
| compiler | `package_id_wamn_is_reserved_for_the_platform` | T2 moves the format refusal to the parsed identifier owner. | `da9f6111a` |
| compiler | `shared_operation_vocabulary_refuses_noncanonical_coordinates` | T2 moves the format refusal to the parsed identifier owner. | `da9f6111a` |
| component-artifact | `a_digest_that_cannot_name_an_artifact_refuses_before_any_transport` | T2 moves the format refusal to the parsed identifier owner. | `deb502710` |
| component-artifact | `invalid_admitted_digest_refuses_before_network_io_without_echoing_it` | T2 moves the format refusal to the parsed identifier owner. | `deb502710` |
| environment | `a_reserved_or_bad_project_id_is_rejected_before_any_effect` | T2 moves the format refusal to the parsed identifier owner. | `5564460da` |
| environment | `pat_prefix_accepts_only_sixteen_lowercase_hex_digits` | T2 moves the format refusal to the parsed identifier owner. | `564c7493f` |
| environment | `the_minted_instance_suffix_is_a_fresh_valid_dns_label_tail` | T2 moves the format refusal to the parsed identifier owner. | `564c7493f` |
| events | `construction_refuses_invalid_event_coordinates_before_connect` | T2 moves the format refusal to the parsed identifier owner. | `5564460da` |
| events | `derived_publication_refuses_a_noncanonical_package_identity` | T2 moves the format refusal to the parsed identifier owner. | `8a9438ca3` |
| identity | `operator_subject_is_exactly_environment_targeted_and_validated` | T2 moves the format refusal to the parsed identifier owner. | `de6502e3d` |
| identity-protocol | `csrf_claim_is_optional_and_refuses_anything_but_a_sha256_hex_digest` | T2 moves the format refusal to the parsed identifier owner. | `deb502710` |
| package-artifact | `a_digest_is_given_in_full` | T2 moves the format refusal to the parsed identifier owner. | `deb502710` |
| package-artifact | `a_tag_follows_the_oci_grammar` | T2 moves the format refusal to the parsed identifier owner. | `564c7493f` |
| platform-install | `reserved_org_id_is_rejected` | T2 moves the format refusal to the parsed identifier owner. | `5564460da` |
| provision | `a_platform_domain_that_is_not_a_domain_name_is_refused` | T2 moves the format refusal to the parsed identifier owner. | `564c7493f` |
| provision | `instance_suffix_discipline_is_the_dns_label_tail` | T2 moves the format refusal to the parsed identifier owner. | `564c7493f` |
| registry | `malformed_projects_have_a_refusal` | T2 moves the format refusal to the parsed identifier owner. | `5564460da` |
| registry | `project_env_key_validation` | T2 moves the format refusal to the parsed identifier owner. | `de6502e3d` |
| registry | `project_validation_is_the_strict_provisioned_name_rule` | T2 moves the format refusal to the parsed identifier owner. | `5564460da` |
| registry | `the_reserved_prefix_stops_at_a_hyphen` | T2 moves the format refusal to the parsed identifier owner. | `5564460da` |
| sql | `a_serving_host_refuses_an_invalid_declared_project` | T2 moves the format refusal to the parsed identifier owner. | `5564460da` |
| sql | `manifest_digest_shape_matches_the_run_plane_check` | T2 moves the format refusal to the parsed identifier owner. | `0423c8f33` |

The W37 draft does not give a separate rationale for `query_errors_keep_the_sqlstate_and_database_message` (`deleted-tests.md::Review limits`).
It also does not separately explain the two old SQL span-map cases or the retained resource-decoding property (`deleted-tests.md`).
These are evidence limits, rather than claims that every surviving property received replacement coverage.

## 4. Changed and merged tests

The first tables record assertion changes through `140a36c2b`.
Each row cites the introducing commit, as recorded in `changed-tests.md`.
Later T3 changes and final names follow below.

| Crate or source owner | Test at the interim head | Change and controlling rule | Commit |
| --- | --- | --- | --- |
| Platform UI | `shows status and selects an application release` | W40 removes the duplicate digest panel. The renamed case now waits for the selected application release and its profile request. | `b81aed759` |
| Platform UI | `expands workload and role details and opens Operations without changing the address` | W40 removes Operations reaches, statements and relation detail rows. The renamed case keeps workload and role detail checks and opening Operations. | `b81aed759` |
| Platform UI | `does not request a profile when no release is installed` | W40 removes the duplicate profile panel. The case drops the panel-absence assertion and keeps the no-profile-request assertion. | `b81aed759` |
| Platform UI | `lets a project admin read its environment but offers no status writes` | W40 changes the awaited UI contract from the removed profile region to the application-release selector. The authority refusal remains. | `b81aed759` |
| Platform UI | `rejects removed reference routes: %s` | W40 removes three parameter cases for nested reach, statement and relation routes. The same callback still refuses missing workload, role and operation routes. The parameter set changes even though the shared expect expressions stay the same. | `b81aed759` |
| Platform UI | `hides controls without authority for the target: %j` | W40 changes the awaited UI contract from the removed profile region to the application-release selector. Deployment-control refusal remains in the parameterized authority cases. | `b81aed759` |
| blobstore | `staging_and_get_repair_use_the_bound_prefix` | Drops exact digest, storage-key, signed URL and cache-header text assertions under the test rules. The renamed case keeps size, staging, repair and deletion behavior. | `219d42da7` |
| catalog | `a_component_descriptor_drops_the_target_and_the_reader_supplies_it` | T1 removes derived serde round-trip and missing/extra-field checks. Descriptor projection and into_admitted checks remain. Evidence: `/var/tmp/wamn-arch/t1-list-final.md`. | `0491e5f4e` |
| catalog | `stored_annotations_refuse_invalid_lifetimes_paths_and_palette_authority` | T1 removes the derived unknown-annotation input from the refusal loop. Registration, path, lifetime and palette checks remain. The test is renamed. Evidence: `/var/tmp/wamn-arch/t1-list-final.md`. | `0491e5f4e` |
| catalog | `lifetime_defaults_and_bounds` | T1 removes derived unknown-field and numeric-type checks. Default lifetime and bounds remain. The test is renamed. Evidence: `/var/tmp/wamn-arch/t1-list-final.md`. | `0491e5f4e` |
| compiler | `every_contract_type_has_its_exact_typescript_spelling` | W44.6 derives the non-null SQL value variants from WIT. Exact TypeScript spellings remain the client contract, rather than a fixed input roster. | `6e4b359fe` |
| compiler | `the_member_name_rule_reverses_every_contract_name` | W44.6 derives member names from host-contract fields. It checks reversibility over that set instead of hard-coded forward spellings. | `6e4b359fe` |
| compiler | `derivation_inherits_fields_from_the_resolved_base` | W37 removes compiled per-statement relation reach assertions. Inherited actor fields and the model refusal remain. | `284f681bd` |
| compiler | `overlay_builds_inherit_owner_fields_from_manifests_with_build_records` | Removes the overlay freshness refusal because review tab 42 deletes freshness checks. Inherited owner fields remain covered. Evidence: `/var/tmp/wamn-arch/out-gen.md`. | `fd0b5059d` |
| compiler | `a_sync_screen_needs_a_feed_on_its_read` | Checks typed admitted operation.sync values instead of JSON property presence or omission. A screen without a feed still produces the typed refusal. | `fd0b5059d` |
| component-policy | `project_policy_is_exact_and_has_no_namespace_wildcard` | W37 removes the WASI patch ceiling. The surviving policy case now exercises higher WASI revisions while preserving exact authored-interface refusal. | `0bcbaf87a` |
| component-policy | `project_policy_refuses_every_unlisted_authority_surface` | Removes the pinned refused-import count and adjusts the exercised authority inputs. The typed UnadmittedImport refusal remains. Evidence: `/var/tmp/wamn-arch/out2-w37-rest.md`. | `772ac4c66` |
| environment | `history_table_creation_waits_for_a_running_prune` | Replaces the pg_locks waiter probe with an observable pending package-application check while retention owns the lock. The application still completes after retention. | `9ff10cf42` |
| environment | `sqlx_commits_files_with_their_history_and_refuses_checksum_drift` | W38 adds checks that transactional and no-transaction migrations stamp the ApplyPackage component principal. Migration history and checksum refusals remain. | `a5667a460`, `9ff10cf42` |
| environment | `a_fresh_install_keeps_control_plane_tables_out` | W42 removes the hand-written application grant surface. The renamed test keeps control-plane table refusal checks. Per-login grant tests now assert refused operations and catalog rules. | `22f4f6f00` |
| environment | `protected_relations_match_reconciled_postgres` | W42 changes shared assertion helpers: removes the identity-issuer table roster, derives application logins from WorkloadRoleFamily, and permits INSERT authority on platform.tx_identity. Portable-data checks remain. | `22f4f6f00` |
| environment | `write_log_retention_expires_completed_and_uncertain_claims_after_thirty_days` | The renamed test now expires old uncertain claims as well as completed claims after thirty days. It retains recent uncertain claims. The commit cites W15 retention, not a new W37 rule. | `9ff10cf42` |
| environment | `author_enable_and_start_leave_the_release_and_the_environment_unchanged` | Removes the pinned initial count of environment facts. Equality with facts observed before authoring, enabling and starting remains. | `9ff10cf42` |
| environment | `the_host_key_reader_accepts_the_rendered_secret_permissions` | Retargets the rendered-secret permission check from the obsolete host chart to the real control service. The key reader and file permission contract remain. The subject changes through the rendering helpers, so this is more than a path correction. | `c65b2d499` |
| execution-host | `native_member_statement_instances` | W37 removes statement-instance membership, substitution refusal and fixed emission-payload assertions. The shared transaction and rollback checks remain. The merge message explicitly moves composition tests to raw batch SQL. | `284f681bd` |
| execution-host | `native_sql_free_command_begins_from_its_transaction_field` | W37 removes assertions about admitted statement-instance maps and empty released statement lists. Host transaction and rollback behavior remain. | `284f681bd` |
| execution-host | `orphan_collector_retries_failed_deletes_and_protects_rows_young_and_unknown_keys` | W39 adds collector assertions that committed staging bytes become a live object and that collection removes the staged object. Existing orphan and live-object protections remain. | `81b8d1ab5`, `e67e6764c` |
| execution-host | `live_set_counter_is_logged_by_its_request_id` | Moves to the host counter-set route. It asserts replay of the stored result, preservation of a later counter value and conflict on a changed request. Old host-numbering and exact JSON assertions go. | `68cc32663` |
| identity | `platform_identity_round_trip_on_postgres` | W42 removes the identity_relations_carry_stamps helper from this test. That helper pins relation names, column descriptions and trigger text. Identity behavior checks remain. | `22f4f6f00` |
| identity-protocol | `public_key_decoding_matches_the_ed25519_vector` | T1 removes derived JWK missing/extra-field checks. The renamed test keeps the custom Ed25519 decoder vector. Evidence: `/var/tmp/wamn-arch/t1-list-final.md`. | `0491e5f4e` |
| postgres-statements | `decode_one_refuses_zero_or_two_rows` | Drops the assertion that a decode error carries an admitted statement identifier. W37 deletes that statement metadata. | `d8274014b` |
| postgres-statements | `postgres_wire_errors_keep_their_type_and_constraint` | Drops the removed row-limit error case. Constraint checks now compare presence rather than the exact constraint value. The commit belongs to the W37 interface rewrite. This also weakens the surviving constraint check. | `d8274014b` |
| provision | `the_stamp_trigger_stamps_guest_writes_on_postgres` | W42 removes the per-login grant roster from this stamp test. The function security and search-path invariants remain. | `22f4f6f00` |
| provision | `the_history_table_function_creates_one_fixed_shape_on_postgres` | W42 removes the explicit function privilege roster. History table shape and function invariants remain. | `22f4f6f00` |
| provision | `the_wamn_app_history_grants_come_from_the_grant_file_alone_on_postgres` | W42 replaces fixed history-function grant lists with derived privilege rules. The system database still refuses application access. | `22f4f6f00` |
| provision | `the_platform_history_stays_out_of_audit_retention_reach_on_postgres` | W42 removes the inline audit-retention table, column and trigger roster checks. The surviving case calls the grant rule helper. Per-login tests own least-privilege coverage. | `22f4f6f00` |
| provision | `the_http_admitter_role_adds_exactly_the_fresh_permission_reads` | W42 replaces the HTTP-admitter table, column, schema and routine rosters. The case retains authenticated read/refusal behavior and forbids routine authority beyond PUBLIC. | `22f4f6f00` |
| provision | `the_identity_reader_can_never_write_identity_and_neither_reader_reaches_the_other` → `the_identity_reader_cannot_write_identity_or_reach_registry` | W42 splits and renames the combined reader case. Identity checks remain in identity_reader_grants. Connection checks move into connection_reader_grants. Derived rules and actual refusals replace pinned privilege rosters. | `22f4f6f00` |
| provision | `the_connection_reader_cannot_read_or_change_credential_metadata_directly` | W42 adds refusals for reading PAT hashes and directly reading or deleting connection rows. These checks come from the split combined reader case. | `22f4f6f00` |
| run-state | `persisted_statuses_round_trip` | T1 removes derived serde status/failure round trips and an enum-decoder refusal. SQL status/failure conversions remain. Evidence: `/var/tmp/wamn-arch/t1-list-final.md`. | `0491e5f4e` |
| schema-introspection | `catalog_without_views_keeps_canonical_bytes` | T1 removes the direct derived deserialization equality. The independently authored canonical byte vector remains. Evidence: `/var/tmp/wamn-arch/t1-list-final.md`. | `0491e5f4e` |
| schema-introspection | `frozen_types_and_closed_defaults_refuse_unsupported_input` | Accepts character varying as Text and the name scalar default alias. Date remains unsupported. These follow the shared type parser that already accepts the aliases. This is an assertion correction, not a product fix. | `fd0b5059d` |
| service edge | `a_bad_integer_override_is_refused` | T1 removes derived TOML unknown-key refusal. The renamed case keeps the product integer-override parser refusal. Evidence: `/var/tmp/wamn-arch/t1-list-final.md`. | `0491e5f4e` |
| service host | `sigterm_drains_calls_then_hands_the_lease_to_a_ready_host` | W44.4 removes the pg_locks waiter helper. It asserts that the next call is pending, then acquires after SIGTERM and never overlaps the first holder. | `4ab80890d` |
| service identity | `admission_stamps_every_bucket_as_the_platform_principal` | W38 removes the assertion that app.user_id clears after admission. Actor stamps and bucket behavior remain. | `a5667a460` |
| sql | `live_an_invalid_timezone_setting_is_refused` | Moves from the deleted host numbering implementation to a real guest SQL interface. It asserts invalid-timezone refusal, accepted POSIX zones and continued use of the transaction. | `2a2671cde` |
| sql | `the_host_owns_the_operation_transaction_and_the_guest_works_in_it` | W37 now refuses operation SQL outside the host transaction. Expected values change to reflect the absent autocommit update. The removed run-stream exclusion check goes. | `d8274014b` |
| sql | `application_query_uses_the_read_login_and_database_grants_refuse_a_write` | W37/W38 add caller identity and READ ONLY refusal checks to dynamic SQL. W41 adds failure and connection-discard checks after a guest transaction end. | `d8274014b`, `219d42da7` |
| sql | `live_guest_cannot_override_the_injected_role_or_user_identity` | W38 allows session settings to change without changing authority. The case now asserts that forged settings leave only the caller row visible. It drops the old SQL-text refusal and escalation control. | `0cae817fb` |
| sql | `live_a_second_project_env_key_is_refused_rather_than_served_the_first_project_env_keys_rows` | Adds typed Infrastructure context and a ConnectionUnavailable source assertion at identity binding. The existing refusal to serve another project remains. | `a5667a460`, `377ae7a62` |
| sql | `live_reads_and_writes_bind_the_executing_principal` | W38 replaces read autocommit with a host read transaction. The renamed case expects the executing principal on both read and write operations. | `0cae817fb` |
| sql | `live_a_transaction_binds_the_executing_operation_and_a_pooled_connection_keeps_none` | W38 expects the executing operation on read transactions as well as writes. The next transaction still carries no stale operation. | `0cae817fb` |
| sql | `live_a_transaction_carries_only_the_application_its_host_bound` | W38 removes app.application mutation-filter assertions. Separate host-bound applications and clearing on the next transaction remain covered. | `0cae817fb` |
| sql | `live_a_read_invocation_transaction_reads_one_snapshot` | W38 assigns a transaction identity on reads. The old no-assigned-xid assertion goes. Isolation and stable read-snapshot assertions remain. | `0cae817fb` |
| sql | `executor_platform_surface_live` | W37 deletes credential_exactness database, membership, ACL and binding probes from this case. Bound/unbound queue visibility and lease behavior remain through the moved Connections owner. | `68cc32663` |

`apps/platform_ui/test/browser.mjs` follows the W40 application-release selector and retains workload and role navigation (`b81aed759`).
Its top-level checks do not add test callbacks, and `4ff965b69` only formats that edit (`changed-tests.md::Browser script`).

The following tables name every later source case absorbed into a surviving table or shared family fixture.
They preserve input distinctions while reducing duplicate setup and test definitions (T3 audits).

### authentication: merged cases

| Former test | Surviving test or fixture | Contract and rule | Commit |
| --- | --- | --- | --- |
| `a_miss_waits_for_the_refresh_in_flight_and_fetches_nothing` | `varied_unknown_ids_share_one_inflight_slot_and_one_attempt_start_interval` | Unknown keys share refresh work and its attempt interval. Rule 1. | `c3fb7275d` |
| `two_warm_caches_refuse_removed_key_with_endpoint_available` | `warm_caches_refuse_removed_keys_by_deadline_with_and_without_the_endpoint` | Warm caches refuse removed keys by their deadline across endpoint availability. Rule 1. | `c3fb7275d` |
| `two_warm_caches_fail_closed_at_deadline_when_endpoint_is_unavailable` | `warm_caches_refuse_removed_keys_by_deadline_with_and_without_the_endpoint` | Warm caches refuse removed keys by their deadline across endpoint availability. Rule 1. | `c3fb7275d` |

### component-artifact: merged cases

| Former test | Surviving test or fixture | Contract and rule | Commit |
| --- | --- | --- | --- |
| `native_root_start_obeys_enclosing_deadline`<br>`native_linked_start_obeys_enclosing_deadline`<br>`native_export_obeys_enclosing_deadline`<br>`native_dispatch_cancellation_releases_capacity`<br>`native_call_deadline_bounds_root_start` | `native_dispatch_obeys_deadlines_and_releases_capacity` | Dispatch obeys deadlines and releases capacity. Rule 1. | `8b98b7ec4` |
| `a_conforming_stub_returns_the_exact_published_bytes` | `registry_pulls_accept_published_bytes_and_refuse_contradictions` | Registry transfer accepts valid bytes and refuses contradictory content. Rules 1 and 2. | `8b98b7ec4` |
| `a_served_body_the_named_digest_does_not_address_refuses_the_pull` | `registry_pulls_accept_published_bytes_and_refuse_contradictions` | Registry transfer accepts valid bytes and refuses contradictory content. Rules 1 and 2. | `8b98b7ec4` |
| `a_manifest_the_registry_cannot_parse_refuses_as_a_contradiction` | `registry_pulls_accept_published_bytes_and_refuse_contradictions` | Registry transfer accepts valid bytes and refuses contradictory content. Rules 1 and 2. | `8b98b7ec4` |

### dev: merged cases

| Former test | Surviving test or fixture | Contract and rule | Commit |
| --- | --- | --- | --- |
| `application_source_edits_rerun_without_additional_component_roots` | `filesystem_events_rerun_for_source_and_ignore_output` | Source edits rerun the watched build. Generated output does not. Rule 1. | `8b98b7ec4` |
| `filesystem_events_map_owned_inputs_and_ignore_generated_outputs` | `filesystem_events_rerun_for_source_and_ignore_output` | Source edits rerun the watched build. Generated output does not. Rule 1. | `8b98b7ec4` |
| `configuration_files_do_not_recursively_watch_their_parents` | `configuration_files_are_observed_with_existing_or_absent_parents` | Configuration watching handles present and absent parents. Rule 1. | `8b98b7ec4` |
| `an_absent_configuration_parent_is_watched_when_created` | `configuration_files_are_observed_with_existing_or_absent_parents` | Configuration watching handles present and absent parents. Rule 1. | `8b98b7ec4` |
| `ignored_build_output_inside_a_component_root_never_reruns_the_loop` | `ignored_build_outputs_never_rerun_overlapping_component_roots` | Build outputs do not retrigger overlapping roots. Rule 1. | `8b98b7ec4` |
| `first_build_outputs_in_overlapping_component_roots_do_not_rerun` | `ignored_build_outputs_never_rerun_overlapping_component_roots` | Build outputs do not retrigger overlapping roots. Rule 1. | `8b98b7ec4` |
| `git_snapshot_refuses_an_initial_repository` | `git_snapshot_covers_the_whole_worktree_and_excludes_ignored_files` | The snapshot test also covers an initial repository. Rule 1. | `8b98b7ec4` |
| `platform_install_recovery_kind` | `platform_apply_kind` | The installation table also covers recovery. Rule 1. | `8b98b7ec4` |

### environment: merged cases

| Former test | Surviving test or fixture | Contract and rule | Commit |
| --- | --- | --- | --- |
| `one_sequence_name_with_two_patterns_in_one_application_is_refused` | `sequence_defaults_use_map_names_and_refuse_conflicting_declarations` | The sequence table preserves name ownership and conflicting-pattern refusals. Rule 1. | `4e856d583` |
| `one_sequence_name_with_one_pattern_in_one_application_is_allowed` | `sequence_defaults_use_map_names_and_refuse_conflicting_declarations` | The sequence table preserves name ownership and conflicting-pattern refusals. Rule 1. | `4e856d583` |
| `one_sequence_name_in_two_applications_is_allowed` | `sequence_defaults_use_map_names_and_refuse_conflicting_declarations` | The sequence table preserves name ownership and conflicting-pattern refusals. Rule 1. | `4e856d583` |
| `a_named_client_selects_its_package` | `a_web_pin_requires_its_package_but_allows_a_different_code_version` | The web selection table retains package selection. Rule 1. | `4e856d583` |
| `secret_files_are_plain_json_and_mode_0600` | `secret_writes_replace_links_without_following_or_sharing_them` | Secret replacement retains private file permissions. Rules 1 and 2. | `4e856d583` |
| `apply_refuses_a_connection_version_missing_one_required_interface` | `apply_refuses_invalid_names_interfaces_environments_and_versions` | The binding refusal table retains the missing-interface case. Rule 1. | `4e856d583` |
| `a_cursor_newer_than_the_watermark_does_not_expire` | `pruning_every_entry_expires_the_cursor` | The cursor table covers old and current cursors. Rule 1. | `4e856d583` |

### execution-host: merged cases

| Former test | Surviving test or fixture | Contract and rule | Commit |
| --- | --- | --- | --- |
| `native_node_success_revokes_invocation_authority`<br>`native_node_initialization_has_no_authority_and_obeys_deadline`<br>`native_node_execution_obeys_enclosing_deadline`<br>`native_node_trap_revokes_invocation_authority` | `native_node_lifetime_revokes_invocation_authority` | The table covers authority lifetime through success, initialization, execution and traps. Rule 1. | `34b3045a3` |
| `native_download_refuses_anonymous_and_session_without_get_before_io` | `native_listener_uses_shared_authentication_and_preserves_file_paths` | The listener test also covers download refusal before IO. Rule 1. | `34b3045a3` |
| `typed_failure_drops_the_entire_owned_result` | `a_later_signing_failure_refuses_the_whole_result` | A signing failure refuses the entire result. Rule 1. | `34b3045a3` |

### http: merged cases

| Former test | Surviving test or fixture | Contract and rule | Commit |
| --- | --- | --- | --- |
| `health_ps256_assertion_verified_by_endpoint` | `fixed_clock_headers_claims_and_scope_separators` | The JWT table covers all retained algorithms. Rule 1. | `56d415835` |
| `health_es256_assertion_verified_by_endpoint` | `fixed_clock_headers_claims_and_scope_separators` | The JWT table covers all retained algorithms. Rule 1. | `56d415835` |
| `health_rs256_assertion_verified_by_endpoint` | `fixed_clock_headers_claims_and_scope_separators` | The JWT table covers all retained algorithms. Rule 1. | `56d415835` |
| `guest_headers_cannot_spoof_the_host_owned_idempotency_key` | `guest_headers_cannot_supply_transport_or_proxy_authority` | Guest headers cannot supply host transport authority. Rule 1. | `56d415835` |
| `guest_method_override_headers_are_removed` | `guest_headers_cannot_supply_transport_or_proxy_authority` | Guest headers cannot supply host transport authority. Rule 1. | `56d415835` |
| `a_guest_supplied_traceparent_is_not_overwritten` | `the_effect_span_is_injected_onto_the_outbound_request` | Trace propagation retains the guest trace case. Rule 1. | `56d415835` |
| `jwt_ps256_locked_fetch_and_401_retry` | `api_401_fetches_once_more_and_returns_the_second_response` | OAuth retry covers PS256 with the existing retry table. Rule 1. | `56d415835` |
| `a_timeout_never_reads_as_a_refusal_before_dispatch`<br>`a_wiring_failure_names_the_refused_effect_and_not_the_command`<br>`the_two_details_that_dispatched_nothing_are_refused_before_dispatch`<br>`a_lost_response_never_reads_as_effect_uncertain`<br>`a_transport_failure_in_flight_never_reads_as_a_timeout` | `http_errors_preserve_effect_outcomes` | Effect outcomes retain refusal, timeout and uncertain-response distinctions. Rule 1. | `56d415835` |

### provision: merged cases

| Former test | Surviving test or fixture | Contract and rule | Commit |
| --- | --- | --- | --- |
| `valid_project_ids_pass`<br>`consecutive_hyphens_are_rejected`<br>`invalid_project_ids_are_rejected_with_a_reason`<br>`reserved_wamn_prefix_is_rejected`<br>`double_hyphen_collision_pair_is_rejected_in_every_component`<br>`underscore_is_not_a_second_collision_vector`<br>`cdc_object_bound_accounts_for_the_instance_suffix` | `project_coordinates_require_a_bounded_resource_name` | The coordinate table covers collision and length refusals. T2 later gives format rules to parsed identifiers. Rules 1 and 2. | `d39c50613` |
| `manifest_unknown_fields_are_refused_by_the_public_strict_parser` | `invalid_manifests_are_refused_at_the_grant_boundary` | The grant boundary refuses invalid authored manifests. Rule 1. | `d39c50613` |
| `semantic_operation_vocabulary_refusal_reaches_the_grant_boundary` | `invalid_manifests_are_refused_at_the_grant_boundary` | The grant boundary refuses invalid authored manifests. Rule 1. | `d39c50613` |
| `noncanonical_package_identity_reaches_the_grant_boundary` | `invalid_manifests_are_refused_at_the_grant_boundary` | The grant boundary refuses invalid authored manifests. Rule 1. | `d39c50613` |
| `publish_refuses_removal_before_deprecation`<br>`publish_refuses_a_new_required_field`<br>`publish_refuses_a_changed_json_type`<br>`publish_refuses_reserved_auth_parameters`<br>`publish_refuses_http_auth_with_another_interface`<br>`publish_refuses_wasm_without_a_digest`<br>`publish_refuses_native_without_a_digest` | `publish_refuses_invalid_definitions` | The publication table preserves definition compatibility and required-data refusals. Rule 1. | `d39c50613` |
| `oauth_client_credentials_secret_rotation_deletes_token`<br>`oauth_client_credentials_auth_rotation_deletes_token`<br>`oauth_authorization_code_auth_rotation_deletes_token`<br>`oauth_authorization_code_secret_rotation_expires_token`<br>`oauth_unchanged_auth_preserves_token_and_discards_pending_authorization`<br>`oauth_rotation_rollback_restores_tokens_and_pending_authorization` | `oauth_rotation_preserves_isolation_and_rollback` | OAuth rotation preserves token invalidation, isolation and rollback. Rule 1. | `d39c50613` |
| `app_refusals_and_catalog_rules` | `support/grants.rs::family` | The login-family fixture covers application refusals and catalog rules. W42 and rule 1. | `d39c50613` |
| `the_connection_reader_cannot_read_or_change_credential_metadata_directly` | `connection_reader_refusals_and_catalog_rules` | The family fixture retains direct credential refusals. W42 and rule 1. | `d39c50613` |
| `control_routes_cannot_write_release_connections_or_bindings` | `control_refusals_and_catalog_rules` | The family fixture covers control writes and organization isolation. W42 and rule 1. | `d39c50613` |
| `a_control_login_reads_and_writes_only_its_own_org` | `control_refusals_and_catalog_rules` | The family fixture covers control writes and organization isolation. W42 and rule 1. | `d39c50613` |
| `executor_platform_refusals_and_catalog_rules` | `the_executor_platform_role_holds_exactly_its_measured_lease_surface` | The shared family fixture covers executor authority. W42 and rule 1. | `d39c50613` |
| `http_admitter_refusals_and_catalog_rules` | `the_http_admitter_role_adds_exactly_the_fresh_permission_reads` | The shared family fixture covers HTTP admission authority. W42 and rule 1. | `d39c50613` |
| `targeted_issuer_grants_execute_on_postgres` | `identity_issuer_refusals_and_catalog_rules` | The issuer family retains targeted execution and refusals. W42 and rule 1. | `d39c50613` |
| `the_identity_reader_cannot_write_identity_or_reach_registry` | `identity_reader_refusals_and_catalog_rules` | The family fixture retains identity-reader refusals. W42 and rule 1. | `d39c50613` |
| `dedicated_session_reader_columns_execute_on_postgres` | `session_role_reader_refusals_and_catalog_rules` | The family fixture retains the session-reader contract. W42 and rule 1. | `d39c50613` |

Setup changes retain the same expected outcomes (`changed-tests.md::Changes that preserve the expected contract`).
They bind required authority, use moved owners or supply valid inputs under rules 1 and 5.

| Tests | Setup change | Commit |
| --- | --- | --- |
| `an_overlay_update_logs_a_diff_that_folds_to_the_effective_row` | Removes a repeated fixture schema check. The business-history checks stay the same. | `9ff10cf42` |
| `registry_configuration_round_trip`<br>`registry_configuration_constraints` | Reads registry configuration through the principal-bound product read helper. Expected configuration and refusals stay the same. | `377ae7a62` |
| `cancellation_retains_registry_inputs_until_cleanup_commits` | Moves registry query text to Registry and aborts the cancellation task before awaiting it. Retained/deleted row expectations stay the same. | `0bcbaf87a`, `1e33ad4c5` |
| `a_second_verb_refuses_until_the_holder_is_gone` | Passes an owned database connection to the moved lifecycle lock. Lock outcomes stay the same. | `0bcbaf87a` |
| `disabling_a_connection_preserves_file_references_and_referenced_delete_refuses`<br>`platform_pat_round_trip_on_postgres` | Executes writes and refusals in principal-bound fixture transactions. Expected row counts and refusal types stay the same. | `9ff10cf42` |
| `a_platform_family_without_project_env_key_context_reads_exactly_what_its_grants_say`<br>`the_project_env_key_targeted_platform_member_reads_nothing_outside_its_grants`<br>`the_guest_family_reads_its_own_project_env_key_and_only_its_own`<br>`the_executor_platform_role_holds_exactly_its_measured_lease_surface` | Binds the required W38 principal around SQL. The existing read/refusal and convergence outcomes stay the same. | `9ff10cf42`, `22f4f6f00` |
| `run_state_live` | Passes explicit project coordinates to queue SQL and uses retention authority. Expected state transitions stay the same. | `a5667a460`, `9ff10cf42` |
| `every_identity_writer_records_a_drop_of_its_principal` | Passes the executing principal to renewal authentication. Recorded authorization-drop expectations stay the same. | `a5667a460` |
| `collection_refuses_objects_from_another_environment_on_the_same_store` | Passes the invocation deadline to the constructor. Cross-environment refusal stays the same. | `81b8d1ab5` |
| `live_compiled_per_user_policy_permits_the_injected_caller` | Changes explanatory diagnostics from app.* settings to protected identity. Row visibility expectations stay the same. | `0cae817fb` |
| `live_a_cold_connection_parses_inside_the_identity_transaction` | Uses streamed row vectors after removal of row-set. Expected row counts stay the same. | `284f681bd` |
| `production_lease_live`<br>`mixed_retries_end_with_the_last_cause`<br>`lease_release_fences_and_bounds_backoff` | Calls the Connections-owned lease wrapper. All lease, retry and backoff outcomes stay the same. | `68cc32663` |
| `connection_shape_refuses_missing_invalid_and_value_fields` | Uses the authored-manifest parser for authored input. Invalid inputs still refuse. | `fd0b5059d` |
| `history_names_are_reserved_and_fit_in_a_postgres_name`<br>`a_command_that_writes_an_actor_field_is_refused`<br>`an_integer_key_cannot_be_numbered_even_with_a_bare_counter`<br>`an_overlay_command_cannot_write_inherited_number_or_actor_fields` | Supplies valid authored SQL and reads typed errors through the current interface. Expected error categories stay the same. | `fd0b5059d` |
| `a_known_result_needs_no_update_and_a_missing_finish_rolls_back` | Renames the known-result case and wraps its fixture SQL in a host transaction. Rollback and stored-result expectations stay the same. | `219d42da7` |

The following former names survive under new names, rather than disappearing as coverage.
Their assertion changes follow the preceding tables and the cited move inventory.

| Former name | Final name | Evidence |
| --- | --- | --- |
| `stored_annotations_refuse_unknown_fields_lifetimes_and_palette_authority` | `stored_annotations_refuse_invalid_lifetimes_paths_and_palette_authority` | `moved-tests.md` |
| `lifetime_and_decoding_fail_closed` | `lifetime_defaults_and_bounds` | `moved-tests.md` |
| `a_fresh_install_grants_the_application_role_invariant_one` | `a_fresh_install_keeps_control_plane_tables_out` | `moved-tests.md` |
| `write_log_retention_removes_old_successes_and_keeps_uncertain_claims` | `write_log_retention_expires_completed_and_uncertain_claims_after_thirty_days` | `moved-tests.md` |
| `kind_needs_no_gcp_resources` | `the_local_provider_needs_no_gcp_resources` | `389bab3c6` |
| `validate_project_env_slug_requires_a_bounded_name` | `project_coordinates_require_a_bounded_resource_name` | `5564460da` |
| `resource_coordinates_round_trip_and_keep_project_databases_separate` | `resource_coordinates_keep_project_databases_separate` | `5564460da` |
| `principal_and_role_parsers_refuse_invalid_values` | `role_parser_refuses_invalid_values` | `5ca0f6c6a` |
| `public_jwk_wire_is_exact_and_rejects_private_or_missing_fields` | `public_key_decoding_matches_the_ed25519_vector` | `moved-tests.md` |
| `pinned_profile_refuses_before_fetch_and_claim_misses_never_gain_authority` | `pinned_profile_refuses_before_fetch_and_preserves_signed_roles` | `c3fb7275d` |
| `staging_hash_size_and_get_repair_use_the_bound_prefix` | `staging_and_get_repair_use_the_bound_prefix` | `moved-tests.md` |
| `an_incomplete_body_is_other_and_says_nothing_was_written` | `an_incomplete_body_is_other` | `moved-tests.md` |
| `a_refused_verb_is_other_and_names_the_verb` | `a_refused_verb_is_other` | `moved-tests.md` |
| `a_lost_body_is_other_and_tells_the_guest_to_re_read` | `a_lost_body_is_other` | `moved-tests.md` |
| `a_published_release_pulls_back_byte_exact_and_loads_the_release_it_names` | `a_published_release_pulls_verified_bytes_and_refuses_an_absent_digest` | `8b98b7ec4` |
| `registered_capabilities_are_platform_imports_whatever_their_namespace` | `registered_packages_are_platform_imports_whatever_their_namespace` | `b52642888` |
| `a_valid_http_snapshot_is_refused_against_another_capabilitys_descriptor` | `a_valid_http_snapshot_is_refused_against_another_services_descriptor` | `b52642888` |
| `reads_authorize_without_a_transaction_and_refuse_begin` | `reads_use_a_host_transaction_and_refuse_write_claims` | `moved-tests.md` |
| `live_a_transaction_binds_the_executing_principal_and_a_read_stays_autocommit` | `live_reads_and_writes_bind_the_executing_principal` | `moved-tests.md` |
| `an_unknown_key_or_a_bad_integer_is_refused` | `a_bad_integer_override_is_refused` | `moved-tests.md` |

### Later changes to surviving contracts

| Crate | Test or interface | Final change and rule | Evidence |
| --- | --- | --- | --- |
| authentication | `pinned_profile_refuses_before_fetch_and_preserves_signed_roles` | Renames the profile case. Keeps signed roles and refusal before fetching. Rule 1. | `c3fb7275d`, `t3b-audit.md` |
| authentication and SQL | Session verifier and permission tests | Share authentication-owned HTTPS support. SQL-only setup stays beside the SQL test. Rule 1. | `c3fb7275d` |
| authority | `a_queued_run_holds_the_operations_of_its_role_only` | Moves to authority unit coverage. Rule 1. | `c3fb7275d` |
| component-artifact | `a_published_release_pulls_verified_bytes_and_refuses_an_absent_digest` | Keeps registry observations. Drops whole-byte equality and redundant load checks. Rules 1 and 2. | `8b98b7ec4` |
| dev | Source selection and bundle tests | Retain source closure and separate route-component pins. Drop diagnostic and whole-byte comparisons. Rule 2. | `8b98b7ec4`, `t3a-audit.md` |
| environment | `application_mutations_require_row_security_in_each_database` | Replaces the old protected-relations name. Derives application schemas and enforces row-security behavior. W42 and rule 2. | `4e856d583` |
| environment | `a_fresh_install_keeps_control_plane_tables_out` | Derives ownership instead of pinning a relation roster. W42 and rule 2. | `4e856d583` |
| environment | `migration_login_cannot_escape_or_change_privileges_even_through_dynamic_sql` | Keeps migration escape refusals. Does not replace them with a renamed expected set. Rule 5. | `4e856d583`, `t3f-audit.md` |
| environment | `reset_preserves_platform_schemas_and_cdc_and_reapplies_package_sql` | Stays as the live interface test of reset. W47 explicitly keeps it. | `rulings.md::W47.3` |
| execution-host | File grant and CSRF cases | Derive file operations from the catalog. Rule 2. | `34b3045a3` |
| execution-host | `status_answers_when_count_reads_fail`, `status_answers_when_count_reads_time_out` | W47 restores both original tests after T3 temporarily merged them. The intermediate `status_keeps_independent_counts_and_zero` goes. | `34b3045a3`, `7f5ae5d93` |
| provision | `connection_routes_refuse_mismatches_and_project_only_enabled_rows` | Keeps route refusals and enabled-row projection. Drops the definer roster and per-table grant loops. W47. | `7f5ae5d93` |
| provision and SQL | `project_schema` and executor-surface cases | Drop unread `app.project_env_key` assertions. Keep database isolation and identity outcomes. W45. | `10a056424` |
| registry | `resource_coordinates_keep_project_databases_separate` | Keeps resource separation after the coordinate round-trip assertions move to parsed types. T2. | `de6502e3d`, `5564460da` |
| identity-protocol | `role_parser_refuses_invalid_values` | Retains the role refusal cases from the combined principal/role test. Principal parsing gets its own table. T2. | `5ca0f6c6a` |
| catalog | `persisted_component_target_refuses_duplicate_fields` | Retains the custom duplicate-field refusal after package formats move to parsed types. T2. | `da9f6111a` |
| compiler | `every_contract_type_has_its_exact_typescript_spelling` | Derives the type set from WIT. Exact spelling remains the client contract. W44. | `6e4b359fe` |
| compiler | `the_member_name_rule_reverses_every_contract_name` | Derives member names from contract fields instead of a hand list. W44. | `6e4b359fe` |
| service identity | Local mail output | Drops recipient, subject and credential-word presence assertions. Keeps local-file behavior and private permissions. Rule 2. | `5ca0f6c6a` |
| engine and HTTP | `registered_packages_are_platform_imports_whatever_their_namespace`, `a_valid_http_snapshot_is_refused_against_another_services_descriptor` | Rename service terminology without changing the refusal contract. | `b52642888` |

T2 also replaces raw fixture identifiers with parsed values across callers (`deb502710`, `de6502e3d`, `5ca0f6c6a`, `da9f6111a`, `564c7493f`, `5564460da`).
Those mechanical call changes do not add new contracts or justify deleting surviving policy refusals (`t2e-inventory.md`, `t2e2-audit.md`).
Relationship, shape, authorization, resource and free-text checks stay at their existing interfaces (`t2e2-audit.md::Not identifiers`).

## 5. Moved tests

S13-N moves consuming tests upward to remove reverse development dependencies (`ce6844f93`, `moved-tests.md`).
The following counts describe that recorded move, before later T3 consolidation.
The HTTP lifetime case subsequently disappears because its feature target never runs (`34b3045a3`).

| Source owner | Destination owner | Definitions at the move | Evidence |
| --- | --- | ---: | --- |
| Broker advisories | Events integration tests | 10 | `moved-tests.md`, `ce6844f93` |
| Broker authorization | Authority integration tests | 1 | `moved-tests.md`, `ce6844f93` |
| Catalog record history | Provision integration tests | 1 | `moved-tests.md`, `ce6844f93` |
| Platform Install chart permissions | Environment integration tests | 1 | `moved-tests.md`, `ce6844f93` |
| Execution Host trusted HTTP route | Workflow integration tests | 1 | `moved-tests.md`, `ce6844f93` |
| Run State admission and persistence | Provision integration tests | 2 | `moved-tests.md`, `ce6844f93` |
| Platform Identity project schema | SQL integration tests | 6 | `moved-tests.md`, `ce6844f93` |
| Platform Identity session keys | SQL integration tests | 3 | `moved-tests.md`, `ce6844f93` |
| Authentication route cases, including support modules | SQL integration tests | 12 | `moved-tests.md`, `ce6844f93` |
| Events transport | Events implementation | 6 | `moved-tests.md`, `ce6844f93` |
| HTTP invocation lifetime | Execution Host integration tests | 1 | `moved-tests.md`, `ce6844f93` |
| SQL executor plugin cases | HTTP integration tests | 2 | `moved-tests.md`, `ce6844f93` |
| SQL production lease cases | Events integration tests | 4 | `moved-tests.md`, `ce6844f93` |

The draft records 50 definitions in the S13-N move, including six with the Events merge (`moved-tests.md`).
It records 1,515 moved definitions through `140a36c2b`, including 1,502 Rust functions and 13 TypeScript callbacks (`counts.md`).
These are interim movement counts, rather than a final-head deletion or execution total.
S13-G supplies most path changes without changing the source owner (`1fa850345`, `7682829c2`, `8d2ee2011`, `ae92d8ce3`).
The before/after owner paths in section 2 show the final layout.

The following table groups surviving directory moves by source and destination.
Counts follow the names in `moved-tests.md` through the final renames and deletions at `60cf3bba8`.
Cross-owner moves remain in the tables above, so these rows describe layout moves within a mapped owner.

| Source directory | Final directory | Surviving moved definitions |
| --- | --- | ---: |
| `apps/platform/data/access` | `crates/data-access` | 17 |
| `apps/platform/data/postgres-statements` | `crates/postgres-statements` | 3 |
| `apps/platform/data/record-history` | `crates/record-history` | 15 |
| `apps/platform/execution/blob-put` | `guests/blob-put` | 3 |
| `apps/platform/execution/jsonata` | `guests/jsonata` | 7 |
| `apps/platform/ingress/http-route` | `guests/http-route` | 17 |
| `apps/platform/no-std/http-page` | `guests/http-page` | 10 |
| `apps/platform/no-std/http-request` | `guests/http-request` | 3 |
| `apps/platform/no-std/label-render` | `guests/label-render` | 1 |
| `apps/platform/no-std/modbus` | `guests/modbus` | 1 |
| `apps/platform/no-std/transform` | `guests/transform` | 3 |
| `apps/platform/wamn_integration/component` | `apps/wamn_integration/component` | 2 |
| `crates/catalog/model` | `crates/catalog` | 98 |
| `crates/catalog/package-artifact` | `crates/package-artifact` | 22 |
| `crates/control/dev` | `crates/dev` | 14 |
| `crates/control/environment` | `crates/environment` | 103 |
| `crates/control/platform-install` | `crates/platform-install` | 39 |
| `crates/control/provision` | `crates/provision` | 67 |
| `crates/control/publish` | `crates/publish` | 61 |
| `crates/control/registry` | `crates/registry` | 1 |
| `crates/control/release` | `crates/release` | 8 |
| `crates/events/wire` | `crates/event-wire` | 6 |
| `crates/execution/host` | `crates/execution-host` | 73 |
| `crates/execution/run-state` | `crates/run-state` | 2 |
| `crates/execution/workflow` | `crates/workflow` | 68 |
| `crates/identity/client` | `crates/identity-client` | 12 |
| `crates/identity/platform` | `crates/identity` | 46 |
| `crates/identity/protocol` | `crates/identity-protocol` | 11 |
| `crates/platform/authentication` | `crates/authentication` | 15 |
| `crates/platform/blobstore` | `crates/blobstore` | 46 |
| `crates/platform/capability-authority` | `crates/authority` | 16 |
| `crates/platform/component-artifact` | `crates/component-artifact` | 6 |
| `crates/platform/component-composer` | `crates/component-composer` | 4 |
| `crates/platform/component-policy` | `crates/component-policy` | 14 |
| `crates/platform/connections` | `crates/connections` | 28 |
| `crates/platform/digest` | `crates/digest` | 4 |
| `crates/platform/engine` | `crates/engine` | 101 |
| `crates/platform/events-capability` | `crates/events` | 4 |
| `crates/platform/http-capability` | `crates/http` | 62 |
| `crates/platform/number-pattern` | `crates/number-pattern` | 3 |
| `crates/platform/postgres` | `crates/postgres` | 5 |
| `crates/platform/postgres-capability` | `crates/sql` | 64 |
| `crates/platform/scalar` | `crates/scalar` | 3 |
| `crates/schema/generator` | `crates/compiler` | 200 |
| `crates/schema/introspection` | `crates/schema-introspection` | 14 |

Additional ownership and tier moves follow.

| Source | Destination | Count | Evidence |
| --- | --- | ---: | --- |
| SQL host numbering request-log case | Execution Host counter-set route | 1 | `moved-tests.md`, `68cc32663` |
| SQL host numbering timezone case | SQL query interface | 1 | `moved-tests.md`, `2a2671cde` |
| Authentication queued-run role case | Authority unit tests | 1 | `t3b-audit.md`, `c3fb7275d` |
| Execution Host session file-grant case | Execution Host library tests | 1 | `t3e-audit.md`, `34b3045a3` |
| Provision portable apply refusal | `tests/apply_grants.rs` | 1 | `t3d-audit.md`, `d39c50613` |
| Control CLI `platform_update` | Platform Install integration tests | 1 | `ownq-audit.md`, `7f5ae5d93` |
| KCL/Helm-dependent library cases | `cluster_document`, `platform_charts`, `platform_show` integration targets | 8 | `kclrt-audit.md`, `389bab3c6` |
| Platform document canonicalization | `cluster_document` integration target | 1 | `kclrt-audit.md`, `389bab3c6` |

The tool-dependent targets state `bin:kcl` and, where required, `bin:helm` (`389bab3c6`).
Their ignore attributes describe prerequisites that the ordinary library tier does not supply, as rule 4 requires (`kclrt-audit.md`).
The canonicalization case needs no tool and retains no ignore attribute (`crates/platform-install/tests/cluster_document.rs`).
`platform_update` keeps a 100 ms timeout for the locked path and five seconds for success (`7f5ae5d93`).
W47 splits per-call scope ownership and merges Integrations into Connections without adding test definitions (`463da0ca5`, `w43s-audit.md`).

## 6. Added tests

New source definitions include replacement privilege interfaces and parsed-identifier tables.
A table that absorbs existing cases appears in section 4, rather than counting each input as a new test.
The first table starts from `counts.md::Added source tests` and records only definitions that survive at `60cf3bba8`.

| File and new test | Contract | Evidence |
| --- | --- | --- |
| `crates/blobstore/src/files.rs::collection_deletes_uncommitted_staging_only_after_the_safety_window` | Uncommitted staged objects survive until the safety window and are then collected. W39. | `21bf607d6` |
| `crates/blobstore/src/files.rs::collection_promotes_staging_with_a_catalog_row_and_removes_staging` | Committed file rows cause staging promotion and staging removal. W39. | `21bf607d6` |
| `crates/blobstore/src/files.rs::stage_deadline_stops_the_upload_and_its_tasks` | The invocation deadline stops upload work and its tasks. W39. | `81b8d1ab5` |
| `crates/blobstore/src/files.rs::collection_reaches_staging_after_the_invocation_time_limit` | Collection reaches abandoned staging after the invocation limit. W39. | `68acd70b2` |
| `crates/compiler/src/generate/sql_layout_tests.rs::authored_sql_accessors_compile_in_both_output_layouts` | Generated accessors compile from both supported output layouts. | `2a2671cde` |
| `crates/compiler/src/manifest.rs::component_connections_validate_declared_aliases` | Component-level connection declarations refuse invalid aliases. W37. | `343e5fb07` |
| `crates/execution-host/src/number_counter_tests.rs::counter_set_refuses_invalid_input` | The host counter-set interface refuses invalid requests. W37. | `68cc32663` |
| `crates/platform-install/src/platform/identity.rs::owner_invitation_serializes_and_refuses_a_different_active_admin` | Owner invitation serializes and refuses a different current administrator. | `772ac4c66` |
| `crates/provision/tests/administration_grants.rs::administration_refusals_and_catalog_rules` | Administration receives only its allowed authority. W42. | `22f4f6f00` |
| `crates/provision/tests/audit_retention_grants.rs::audit_retention_refusals_and_catalog_rules` | Audit retention cannot reach unrelated protected data. W42. | `22f4f6f00` |
| `crates/provision/tests/connection_reader_grants.rs::connection_reader_refusals_and_catalog_rules` | The connection reader cannot directly mutate credential metadata. W42. | `22f4f6f00` |
| `crates/provision/tests/control_grants.rs::control_refusals_and_catalog_rules` | A control login stays within its organization and allowed writes. W42. | `22f4f6f00` |
| `crates/provision/tests/deploy_sql_authority.rs::identity_grants_refuse_guest_reads_and_mutations` | Guest authority cannot read or mutate protected identity state. W38. | `0cae817fb` |
| `crates/provision/tests/deploy_sql_authority.rs::the_first_identity_insert_wins_and_other_transactions_refuse` | The host identity insert wins. Another transaction cannot supply the row. W38. | `a5667a460` |
| `crates/provision/tests/deploy_sql_authority.rs::history_keeps_the_host_actor_after_a_guest_forges_a_setting` | History retains the host actor after forged session settings. W38. | `a5667a460` |
| `crates/provision/tests/deploy_sql_authority.rs::a_project_policy_refuses_without_a_transaction_identity` | Row policy refuses a call without its transaction identity. W38. | `a5667a460` |
| `crates/provision/tests/identity_issuer_grants.rs::identity_issuer_refusals_and_catalog_rules` | The issuer receives only its required signing and identity authority. W42. | `22f4f6f00` |
| `crates/provision/tests/identity_reader_grants.rs::identity_reader_refusals_and_catalog_rules` | The identity reader cannot write identity or read registry state. W42. | `22f4f6f00` |
| `crates/provision/tests/migration_grants.rs::migration_refusals_and_catalog_rules` | The migration login cannot escape to privileged platform roles. W42. | `22f4f6f00` |
| `crates/provision/tests/platform_logins_grants.rs::project_platform_logins_cannot_escalate` | Project logins cannot escalate through role attributes or membership. W42. | `22f4f6f00` |
| `crates/provision/tests/platform_logins_grants.rs::system_platform_logins_cannot_escalate` | System logins cannot escalate through role attributes or membership. W42. | `22f4f6f00` |
| `crates/provision/tests/query_grants.rs::query_refusals_and_catalog_rules` | The read login refuses writes except the required identity insert. W42. | `22f4f6f00` |
| `crates/provision/tests/retention_grants.rs::retention_refusals_and_catalog_rules` | Retention stays within its deletion authority. W42. | `22f4f6f00` |
| `crates/provision/tests/service_reader_grants.rs::service_reader_refusals_and_catalog_rules` | The service reader cannot gain write authority. W42. | `22f4f6f00` |
| `crates/provision/tests/session_role_reader_grants.rs::session_role_reader_refusals_and_catalog_rules` | The session reader receives only permission-read authority. W42. | `22f4f6f00` |
| `crates/sql/src/query_tests.rs::guest_commit_fails_host_finish_and_discards_the_connection` | A guest transaction end fails host finish and prevents connection reuse. W41. | `219d42da7`, `2a2671cde` |
| `crates/sql/src/session_identity/tests.rs::live_operation_reads_keep_identity_and_refuse_writes` | Read operations retain caller identity and refuse writes. W38. | `535c4843a` |

W42's application, executor-platform and HTTP-admitter standalone cases later merge into family fixtures and existing tests (`d39c50613`).
Their refusal coverage remains in the family setup and the named surviving cases in section 4 (`t3d-audit.md`).
They are not final additions in the table above.

The following identifier tables test the public constructors once at their owning interface.
They also exercise boundary conversions where the source table supplies them (T2 commits, `t2e-inventory.md`).

| Owner and test | Parsed contract | Commit |
| --- | --- | --- |
| `crates/digest/src/lib.rs::parse_rule` | Digest accepts only the canonical SHA-256 spelling. | `deb502710` |
| `crates/registry/src/env_name.rs::parse_rule` | EnvName owns environment-name syntax. | `de6502e3d` |
| `crates/registry/src/project_env_key.rs::parse_rule` | ProjectEnvKey owns the composite environment key. | `de6502e3d` |
| `crates/identity-protocol/src/principal.rs::principal_parse_rule` | PrincipalId owns the canonical principal UUID. | `5ca0f6c6a` |
| `crates/identity-protocol/src/login.rs::login_parse_rule` | LoginName owns PostgreSQL login text constraints. | `5ca0f6c6a` |
| `crates/catalog/src/package.rs::parse_rule` | PackageName owns package syntax and the reserved platform name. | `da9f6111a` |
| `crates/catalog/src/environment_application.rs::parse_rule` | ApplicationName owns nonempty SQL text without NUL. | `da9f6111a` |
| `crates/registry/src/instance_suffix.rs::parse_instance_suffix` | InstanceSuffix owns the database instance suffix. | `564c7493f` |
| `crates/registry/src/platform_domain.rs::parse_platform_domain` | PlatformDomain owns domain syntax. | `564c7493f` |
| `crates/registry/src/integration_code.rs::parse_integration_code` | IntegrationCode owns implementation-code syntax. | `564c7493f` |
| `crates/identity-protocol/src/pat_prefix.rs::parse_pat_prefix` | PatPrefix owns the lowercase token lookup prefix. | `564c7493f` |
| `crates/package-artifact/src/tag.rs::parse_oci_tag` | OciTag owns OCI tag syntax and length. | `564c7493f` |
| `crates/catalog/src/client_package_name.rs::parse_client_package_name` | ClientPackageName owns npm client package syntax. | `564c7493f` |
| `crates/catalog/src/registered_operation.rs::parse_registered_operation` | RegisteredOperation owns versioned operation-export syntax. | `564c7493f` |
| `crates/catalog/src/local_operation.rs::parse_local_operation` | LocalOperation owns package-local operation identity. | `564c7493f` |
| `crates/catalog/src/package_version.rs::parse_package_version` | PackageVersion owns canonical authored version text. | `564c7493f` |
| `crates/registry/src/org_id.rs::parse_rule` | OrgId owns organization-coordinate syntax. | `5564460da` |
| `crates/registry/src/project_id.rs::parse_rule` | ProjectId owns project-coordinate syntax. | `5564460da` |
| `crates/compiler/src/source_name.rs::parse_rule` | SourceName owns authored SQL identifier syntax. | `5564460da` |
| `crates/catalog/src/canonical_name.rs::parse_rule` | CanonicalName owns canonical catalog and wiring names. | `5564460da` |

Additional boundary tests cover new privilege and JSONata rules.

| File and new test | Contract | Evidence |
| --- | --- | --- |
| `crates/provision/tests/app_grants.rs::application_logins_cannot_reach_mapped_platform_logins` | No application or query login can reach a mapped platform login through role membership. W45. | `10a056424` |
| `crates/provision/tests/platform_logins_grants.rs::platform_security_definers_have_restricted_authority` | Every system-database platform definer belongs to its database owner, uses `pg_catalog`, denies PUBLIC EXECUTE and has no grantable execution. W47/W48. | `7f5ae5d93`, `e75b18cef` |
| `crates/provision/tests/platform_logins_grants.rs::project_platform_security_definers_have_restricted_authority` | The same derived invariant applies in a project database. W48. | `e75b18cef` |
| `crates/catalog/src/wiring_compatibility.rs::publication_checks_the_declared_jsonata_input_and_output_schemas` | JSONata ports match both declared schemas, and returned samples must satisfy the output schema. W46/W47. | `508f0b954`, `1d994dee0` |
| `crates/catalog/src/wiring_compatibility.rs::publication_allows_sample_errors_but_refuses_an_output_outside_the_schema` | A sample evaluation error does not refuse publication. A returned value outside the output schema does. | `b1ce393a4` |
| `crates/workflow/src/graph/walk_tests.rs::target_port_refuses_a_value_before_invocation` | Invalid values fail the target port schema before invoking the target. | `508f0b954` |
| `crates/workflow/src/graph/walk_tests.rs::jsonata_output_must_match_its_declaration_before_routing_or_completion` | JSONata validates its declared output before downstream routing or completion. W47. | `1d994dee0` |
| `crates/catalog/src/component_facts.rs::persisted_component_target_refuses_duplicate_fields` | The custom component-target decoder refuses duplicate fields after typed package parsing. | `da9f6111a` |

The last row replaces the earlier persisted-coordinate case's format assertions and preserves a custom decoder refusal (`da9f6111a`).
W48 derives the definer owner from `pg_database.datdba`, rather than assuming the same role name in both databases (`e75b18cef`).
This resolves the project-owner question in `ownq-audit.md` and supersedes its system-only coverage limit (`defn-audit.md`).
## 7. Product defects found and fixed by the test work

The first table follows `product-fixes.md` through its interim head.
Its source logs establish the listed failures, with the stated limits.
A fix commit does not establish a passing selection at `60cf3bba8` (`product-fixes.md::Remaining evidence limits`).

| Exposing interface or target | Defect and product correction | Commit |
| --- | --- | --- |
| Environment registry configuration tests | Protected configuration reads lacked the provisioning principal. `read_configuration` now opens a principal-bound read transaction. | `377ae7a62` |
| Compiler custom-operation vocabulary case | SQL analysis hid the intended vocabulary refusal. Validate the vocabulary first. | `fd0b5059d` |
| Compiler undeclared-relation case | SQL analysis collapsed an unknown relation into a generic operation error. Preserve the contextual `UnknownRelation` type. | `fd0b5059d` |
| Generated application and control clients | Permission-denied responses omitted the operation. Include it so clients can classify the refusal. | `fd0b5059d` |
| Repeated platform provisioning and initialization | Platform logins lacked the correct fallback principal mapping. Map migration and system logins to their owning component. Roll back failed installation before role reset. | `9ff10cf42` |
| Transactional and no-transaction migrations | History attribution could not rely on a host transaction across migration boundaries. Use protected migration-login mappings. | `a5667a460`, `9ff10cf42` |
| Protected provisioning readers | Org placement and environment policy reads lacked a principal. Bind Provisioning for those reads. | `9ff10cf42` |
| Terminal-run retention | Deletes lacked retention authority. Run preview and deletion in an AuditRetention transaction. | `9ff10cf42` |
| Provision integration/history targets | Schema installation attempted role creation through an unprivileged login. Provision SQL roles before installation. Later W44 moves application grants to default privileges. | `9ff10cf42`, `bd4a9494a` |
| `control_mine_admits_only_a_current_project_admin_or_org_admin_session` | ControlMine read protected authority without the caller identity. Use one caller-bound read transaction. | `9ff10cf42` |
| Identity environment discovery | The issuer lacked the registry columns needed for discovery. Grant those selected columns. The draft links this to a target group. | `9ff10cf42` |
| Signer backend-loss case | The error path returned before transaction rollback completed. Roll back explicitly. The draft retains no isolated before/after result. | `9ff10cf42` |
| Application activation | Initialization expected resolved migration documents but received a URL. Resolve credentials at CNPG callers and pass explicit login credentials elsewhere. | `31f802b56` |
| Warm native credential recovery | Refusal left the selected instance reusable. Retire it through admission preparation and build a fresh instance on recovery. | `377ae7a62`, `c6b9befea` |
| First package publication | An absent registry repository made tag listing fail. Treat that repository as an empty tag set. | `c65b2d499` |
| Development-tag pruning | Registry tag pagination failed to advance. Follow pagination links in the shared transport. | `c65b2d499` |
| Integration restart guest build | A virtual workspace has no root package. Select the package by its canonical manifest path. | `c65b2d499` |
| REST/NetSuite publication | Private operations carried committed-result schemas without registered operations. Emit those schemas only for registered public operations. | `c65b2d499` |
| Edge PAT refusal | An unsupported credential became a 503. Return the intended unauthorized response. | `c65b2d499` |
| Platform UI deploy request | The request omitted the application. Require and send it. | `c65b2d499` |
| Platform UI stop action | The UI omitted stop and its administration authority. Add the action and send the application without a release. | `c65b2d499` |
| Standalone engine build | The wash-runtime feature set omitted its required SHA-256 dependency. Enable the upstream OCI feature. | `c65b2d499` |
| Rendered control key permissions | The real control Secret lacked the accepted file mode. Set 0600 and render the owning chart in the test. | `c65b2d499` |
| Cluster document compilation | Some callers searched PATH alone and missed the pinned KCL installation. Use the shared resolver. | `6a32c08cb` |

Review also produced interface regression coverage.
The draft distinguishes these from defects first exposed by an acceptance failure (`product-fixes.md::Review defects with interface regression coverage`).

| Interface | Defect and correction | Commit |
| --- | --- | --- |
| Guest transaction end | A replacement transaction could pass host finish. Require the original host transaction identity and discard the connection on mismatch. | `219d42da7`, `2a2671cde` |
| Generated SQL accessors | Include paths failed from one emitted layout. Derive them from the emitted file and compile both consumers. | `2a2671cde` |
| Guest numbering timezone | An invalid timezone aborted the transaction. Validate it before evaluating the expression and retain POSIX-zone support. | `2a2671cde` |
| First owner invitation | Invitation deletion lost administrator refusal and serialization. Restore both at the administrator-grant owner. | `772ac4c66` |
| WAL position during an operation | A second connection acquisition could stall while the operation held its connection. Read through that open connection. | `d8274014b` |

Later commits add these corrections beyond the first draft.

| Interface or target | Defect and correction | Commit |
| --- | --- | --- |
| Application activation and reset | Schema creation and grants had conflicting owners. The migration role creates model schemas, provisioning supplies default privileges, and reset restores CDC. | `bd4a9494a`, `dd9b05c9f`, `ffc63be8d` |
| OAuth authorization callback | The pending authorization needed an organization-scoped locking function. Use `registry.lock_oauth_authorization` and grant it through provisioning. | `dbadf6d22` |
| Resource lease cancellation | Polling continued after cancellation. Stop polling when the caller cancels. | `dbadf6d22` |
| CDC reader | TLS mode conversion and missing-slot handling broke the live path. Convert the mode and refuse a missing replication slot. | `dbadf6d22` |
| Guest build identity | Absolute checkout paths changed component identity. Build from one source view below the guest workspace root. | `7866fc6dd` |
| Edge refusal and restart | Refusal returned before SQLite closed. Wait for the handle to close on error as well as success. | `05a03f9c3` |
| Integration publication | Connector inputs named copied models instead of their base models. Reference the base-owned integration models. | `05a03f9c3` |
| Control CLI TLS | Two compiled providers had no selected default. Install AWS-LC once at worker startup. | `389bab3c6` |
| JSONata guest compilation | A catalog constant pulled native cryptography into the WASI build. Keep the guest's port constant beside its declaration. | `00b8d68c8` |
| Derived definer invariant | Three registry policy helpers allowed PUBLIC EXECUTE. Revoke it and preserve explicit grants at the owning role sites. | `7f5ae5d93` |
| JSONata publication samples | Evaluation errors were treated as invalid outputs. Skip sample evaluation errors, but refuse every returned value outside the declared output schema. | `b1ce393a4` |

`w46in-audit.md` reports four nodes blocked by sampling, and `w46b-audit.md` describes provisional empty-result behavior.
The final `b1ce393a4` diff supersedes that provisional account for the two `rows` expressions.
They raise an evaluation error on malformed page shapes, which publication permits and runtime routes through the error port (`b1ce393a4`).
The commit records all four nodes passing the focused publication check, without establishing a full selection result (`b1ce393a4`).

Runner fixes also affect whether test results can be trusted.

| Failure | Runner correction | Commit |
| --- | --- | --- |
| Serial selection exceeded the completion bound | Build selected executables once and schedule independent targets with target and selection deadlines. | `1e33ad4c5` |
| Cancellation left descendants and mixed output | Supervise and reap descendants. Keep output per target. | `ef6cbfc04` |
| A rebuilt xtask path could no longer start its supervisor | Launch the running executable through `/proc/self/exe`. | `140a36c2b` |
| A missing test executable could disappear from the result | Fail the selection when a selected target lacks an executable. | `c78431ff9` |
| Isolated checks and command suites delayed the selection | Run independent isolated checks and command suites beside test targets. | `17104c9fd` |

The session fixture audience correction is a test-fixture fix, rather than a product fix (`60cf3bba8`).
It follows the product's organization/environment/project order, so the permission-restoration case no longer rejects its own token (`60cf3bba8`).
Principal fixture rows, cancellation-task cleanup, guest preparation and refreshed artifact pins also do not count as product behavior fixes (`product-fixes.md::Corrections that are not product fixes`).
The stale run-state lint expectation changes no product behavior (`product-fixes.md`).

## 8. Why ignored tests and support lines did not fall more

Ignored attributes fell by five overall, although several source owners gained them (`raw-base.txt`, `raw-head.txt`).
Most increases reflect moved resource tests or declared tool needs, rather than new disabled coverage (`moved-tests.md`, `389bab3c6`).
The table includes every owner with a positive net ignore change after rename mapping.

| Owner | Before | After | Change | Cause and evidence |
| --- | ---: | ---: | ---: | --- |
| SQL | 4 | 13 | +9 | Authentication and identity database cases move here. The timezone case declares its generated guest input. `moved-tests.md`, `11815429d`. |
| Events, including the old transport crate | 1 | 10 | +9 | Broker advisory tests move to the consuming crate. `ce6844f93`, `moved-tests.md`. |
| Platform Install | 2 | 8 | +6 | KCL and Helm cases declare their tools. Two document snapshots later go. `6a32c08cb`, `389bab3c6`. |
| Authority | 2 | 3 | +1 | The Broker authorization test brings its NATS prerequisite. `ce6844f93`. |
| Workflow | 0 | 1 | +1 | The trusted HTTP guest test moves from Execution Host with its prepared-artifact needs. `ce6844f93`. |
| Control CLI | 4 | 5 | +1 | `platform_show` declares KCL after leaving the library tier. `389bab3c6`. |
| Journeys | 7 | 8 | +1 | The write-log case declares its existing cluster requirements. `6e0ce5e7e`. |

Source counts for each row come from `raw-base.txt` and `raw-head.txt`.
The matching decreases include moved Broker and Authentication cases and deleted or merged resource cases (the same inputs, T3 audits).
Moving a database or cluster test does not remove its prerequisite (`CLAUDE.md::Test rules`, rule 4).
The final selection result remains reserved for root in section 10.

Support source rose by 604 lines overall, despite the large Environment fixture deletion (`raw-base.txt`, `raw-head.txt`).
The script classifies support by its path, so extracting a helper from an inline test module can increase this measure without adding a contract (`counts.md`).
The following table includes every positive support change after mapping source owners.

| Owner | Before | After | Change | Cause and evidence |
| --- | ---: | ---: | ---: | --- |
| SQL | 237 | 1,208 | +971 | Database authentication cases bring their route fixtures from Authentication and Identity. Later T3 deletes SQL's duplicate session fixture. `ce6844f93`, `c3fb7275d`. |
| HTTP | 0 | 869 | +869 | Extracted HTTP test setup becomes support source. T3 later shares TLS setup and removes duplicate helpers. `ce6844f93`, `56d415835`, `t3c-audit.md`. |
| Provision | 258 | 593 | +335 | Moved persistence/history cases and derived family fixtures need database setup. T3 shares that setup and deletes the old system-reader support. `ce6844f93`, `22f4f6f00`, `d39c50613`. |
| Authentication | 107 | 265 | +158 | One authentication-owned HTTPS/session fixture replaces separate copies. `c3fb7275d`, `t3b-audit.md`. |
| Registry | 0 | 167 | +167 | Lifecycle-lock test support moves with its Registry owner. `ce6844f93`. |
| Dev | 3,197 | 3,306 | +109 | Deployment fixtures follow the current product path and typed identifiers while watcher cases merge. `8b98b7ec4`, `5564460da`, `crates/dev/tests/support/`. |
| Connections | 190 | 200 | +10 | Lease fixture support follows typed identifiers and the current caller scope. `463da0ca5`, `5564460da`, `crates/connections/tests/support/`. |
| Control CLI | 170 | 173 | +3 | Fixture construction carries typed environment and principal values. T2 commits, `services/ctl/tests/support/`. |
| Shared application support | 349 | 352 | +3 | Fixture helpers carry parsed package, operation and digest values. T2 commits, `test-support/application/`. |
| Connection HTTP guest fixture | 0 | 125 | +125 | S13-G moves the fixture under test-support. The script reclassifies its existing source. `1fa850345`, `counts.md`. |
| Private Postgres support | 825 | 837 | +12 | Test database setup supplies protected identity mappings. `a5667a460`, `9ff10cf42`, `test-support/postgres/`. |
| Shared resource support | 1,953 | 1,968 | +15 | Resource and package preparation follows moved paths and parsed artifact identities. `1fa850345`, `deb502710`, `test-support/resources/`. |
| Journeys | 163 | 164 | +1 | The write-log support case adds its prerequisite declaration. `6e0ce5e7e`. |

Source counts for each row come from `raw-base.txt` and `raw-head.txt`.
Environment support falls from 4,282 to 2,192 lines after unused file, rendering and document fixtures go (`raw-base.txt`, `raw-head.txt`, `4e856d583`).
Execution Host, Compiler, Edge and package-fixture support also fall in the final raw counts (the same inputs).
The net figure measures the remaining source, rather than a claim that every helper addition offsets a deleted test (`counts.md`).

## 9. Open failures and gaps

The tracker lists the following beads as open on 2026-11-16 (`bd show`).

- `wamn-eulfr.12.5`: The deploy routes have no alias or sequence override for a release. This blocks `a_route_answers_every_operation_type_consistently_across_aliases`.
- `wamn-eulfr.12.6`: The deploy routes have no history expiry trigger. This blocks `the_feed_of_a_list_read_follows_its_snapshot`.
- `wamn-eulfr.12.8`: Two tests of removed native ownership have no product subject. The owner decides whether to delete or rewrite `product_dev_command_applies_packages_and_cleans_up` and `development_identity_survives_schema_reset_and_owned_teardown`.
- `wamn-eulfr.12.9`: The deploy routes have no product data import for test fixtures. This blocks 13 tests that S10-D moved.
- `wamn-eulfr.12.10`: Kind tests need fault injection for an open transaction, statement statistics, an issuer fault, a host crash and a broker stop.
- `wamn-eulfr.12.13`: The W23 kind measurement stopped because the `http-route` guest did not compile after S7-D. The guests need a rebuild before the measurement can run.
- `wamn-eulfr.15.2`: The S13-D baseline had six live failures in the environment and execution-host targets.
- `wamn-eulfr.15.4`: The baseline at `be073d9fa` had live failures in control-provision, environment and platform-install. The activation path needed resolved migration credentials (`product-fixes.md`, `31f802b56`).
- `wamn-eulfr.16`: The root library tests at `109d11175` had 56 failures in 6 crates.
- `wamn-eulfr.32`: `deploy/sql/postgres-init.sql` creates test tables in the production init SQL. The owner ruled W37 "report, do not fix".
- `wamn-eulfr.39`: One rebuild of all apps gave a different component digest for `platform_fixture`, so a build was not a function of its source once. Section 10 has the details.

The drafts also leave evidence gaps for warm-instance recovery and signer rollback (`product-fixes.md`).
`c6b9befea` fixes warm retirement, but the draft's retained focused run stopped earlier at authentication (`product-fixes.md`).
The definer-owner question is resolved by W48, and the four JSONata sample refusals are resolved in the later focused probe (`e75b18cef`, `b1ce393a4`).
`w46b-audit.md` still requires regenerated NetSuite and REST consumer artifacts and reports missing generated WIT inputs for their guest checks.
Their reported limitation is not a passing final guest build or selection result (`w46b-audit.md`).

## 10. Selection result

The owner allowed one full acceptance selection after the restructure. It ran `cargo xtask test-changes run --base 7b9134a7b` at `f9be11e14` (`/var/tmp/wamn-arch/sel-pg-iface.txt`).
It ran from 03:46 to 04:09 UTC on 2026-11-16.
The run took 1,354 seconds, under the 30-minute limit.

Two earlier tries stopped in setup, before any test ran.
The code generator printed the new typed names with Rust debug format, so the generated guest code named `LocalOperation`, `RegisteredOperation` and `SourceName` and did not compile.
`78081c6ea` and `f9be11e14` fixed the generator, and all 12 apps then built.

| Result | Count |
| --- | ---: |
| Test target results | 211 |
| Passed tests | 1,501 |
| Failed tests | 69 |
| Failed tasks that are not tests | 9 |

Four standalone package checks and `compile-packages-alone` failed for one cause.
The other failed tasks are the `web` lint, the `@wamn/platform-ui` lint and test, and the `http-route` guest test build.

The failures fall into these causes.

| Cause | Failures |
| --- | ---: |
| Test fixtures build identifiers that the new parsed types refuse (`ParseOrgIdError`, `ParsePackageNameError`, `ParseCanonicalNameError`, manifest names, registered actions) | 27 |
| SQL binds text where a column is `uuid`, or a fixture writes a digest without `sha256:` | 17 |
| Known deploy route gaps with open beads (`wamn-eulfr.12.5`, `.12.6`, `.12.8`, `.12.9`, `.12.10`) | 8 |
| Other single causes (client plan field order, reset retention, ACL role, trigger function call, project create refusal, OAuth callback login, shutdown polling) | 8 |
| The TypeScript client emitter writes the type name `RegisteredOperation` into generated clients | 4 |
| Build defects (stale guest lock copy, workspace paths in a copied fixture) | 3 |
| REST connector workflow and unit fixture after the W46 page assertion | 2 |

The kind stage did not run. The owner reviews this report before any kind run.

### Fixes after the selection

Root answered the failures with fix lanes and single-case reruns, and did not rerun the full selection.
Each lane ran only its own failing cases, one at a time, each under 5 minutes.

| Commit | Change | Kind |
| --- | --- | --- |
| `6ee665278` | The TypeScript emitter prints the operation as a string. Client fields follow the documented path order. Compiler fixtures use valid identifiers. | Product and fixtures |
| `e8cebc7ee` | `wamn-catalog` asks for the tokio `macros` feature. `GuestWorkspace` resolves its copied lock offline. The `rest_item` workflow refuses a failed consumer outcome before `finish`. Wiring errors print plain names. | Product |
| `07a210d7a` | OAuth callback cases use the org control login. The shutdown case keeps polling during setup. | Fixtures |
| `1cdb3d340` | Project create answers the declared `invalid_input` refusal for a bad name. File, connection, password and loader fixtures bind valid values. | Product and fixtures |
| `6a1eca9ea` | The platform owner owns model schemas (tab 68). One function grants model schema access, and reset restores it in the same transaction. Environment and provision fixtures use product values. | Product and fixtures |
| `90354ec53` | The overlay fixture applies `postgres-init.sql` with psql. The restart fixture keeps crate paths in the repository. | Fixtures |
| `be26f3872` | The manual run workflow declares its JSONata schemas (W46, W47). | Fixture |
| `139fd6d77`, `be10760d7` | The REST consumer fixture installs through `install_project` and stops pinning column privileges, because W44 names `application_activation_live` as the test of those grants. | Fixture |
| `3b4f249b4`, `7b24a3c55` | Clippy errors in `wamn-sql` and `wamn-environment`. | Cleanup |

After these commits, every failed case passes singly, except the 8 cases of the known deploy route gaps.
The overlay compatibility case now stops at `wamn-eulfr.12.10`, which is the same known gap.
The `http-route` adversarial build did not fail again in the lane.
The REST connector unit case needs generated WIT inputs and was checked through its expression only.

One rebuild of all apps gave a different component digest for `platform_fixture` once, and three later builds gave the committed digest.
That is bead `wamn-eulfr.39`.

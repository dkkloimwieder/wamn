# Test practice review

Epic `wamn-inmae`, issues `.2` and `.4`. This is a report. It changes no test and ran no test, build or cluster. The owner rules on each proposal before any test is deleted.

The audit baseline is `788cdadb4` ([measurements](measurements.md) line 3). Source citations are at main `c6233b962`. Read-only Codex gpt-6.1-sol lanes measured the files, and root checked the citations against source.

The review applies the standing testing rulings:

1. Do not test generated or static artifacts outside the generator's own tests.
2. A review is a list of actions on named targets.
3. No incidental counts.
4. A test proves a contract, not an implementation choice.
5. Per-issue cost is build, Clippy and unit tests.

## Part A: the four files that grew

Line ranges include both ends. A "keep" block proves a contract that no lower tier proves. A "delete" block names the test that keeps the contract after the cut.

### crates/control/lib/tests/platform_apply_kind.rs

The file has 2,819 lines today and 2,213 at the audit. The diff since the audit adds 1,929 lines and removes 1,323 (3,252 changed, 606 net). Commit `003eccd57` (a failed cluster resumes at a named step) changed 3,259 of those lines. It moved existing checks into named step blocks, and it added the resume orchestration at lines 65-141, 198-275 and 537-551.

The blocks that prove deployed contracts stay. Examples are database and NATS readiness (644-668), the anonymous NATS refusal (710-740), one NATS (782-803), certificate issuance by cert-manager (804-823), the unqualified-apply refusal (881-899, 936-962), the deployed release digest (1139-1156), the unchanged-state blocks of the rerun (1441-1524), the HTTPS probes (2192-2393) and the log privacy checks (1668-1730, 2442-2503).

The cuts below remove rendered shape, progress narration and log prose. They total 506 lines.

| Lines | Count | What it pins | Owner that keeps the contract |
| --- | ---: | --- | --- |
| 181-195, 592-595, 994-997, 1415-1440, 2776-2815 | 89 | Step titles, status words, timing syntax (`STEP_TITLES`, `require_steps`) | `crates/control/lib/src/platform/install.rs:1567`, `crates/control/lib/src/platform/render.rs:736` |
| 583-591, 639-641 | 12 | Web artifact build and push in a case with no web bucket | `crates/control/lib/src/web_artifact.rs:1036`, `:1342`, `crates/control/lib/src/environment/clients.rs:117` |
| 669-709 | 41 | A list of installed objects with no behavior | `crates/control/lib/tests/platform_render.rs:344` |
| 741-781 | 41 | NATS authorization text | `crates/control/lib/src/platform/install.rs:1532`, `crates/control/lib/src/platform/broker.rs:483` |
| 833-843 | 11 | The release digest found in log prose | The completion check at 844-861 |
| 901-924 | 24 | Qualification refusal prose | `crates/control/lib/tests/release_qualification_live.rs:48`, `crates/control/lib/src/platform/environments.rs:529` |
| 967-981 | 15 | Connection behavior read from `done` and `skipped` prose | The Secret check at 982-993, `crates/control/lib/tests/platform_render.rs:1280` |
| 998-1010 | 13 | Publication output format | 844-861 and 1011-1022 |
| 1117-1138 | 22 | Progress titles of the qualified apply | The deployed-state blocks at 1139-1249 |
| 1157-1168, 1821-1889 | 81 | Number sequence, and its helper `number_sequence_pattern` | `crates/control/lib/tests/run_plane_live/platform_configuration.rs:212` |
| 1192, 1195, 1227, 1459-1461, 1936-1954, 2404-2430 | 52 | Exact skip messages, and the three skip helpers | `crates/control/lib/src/platform/web.rs:325`, `crates/control/lib/src/platform/edge.rs:495`, and the no-Job blocks at 1527-1658 |
| 1250-1306 | 57 | Edge image, service account, selector and Role rule | `crates/control/lib/src/platform/edge.rs:460` |
| 1338-1359 | 22 | The whole invitation success message | `crates/control/lib/src/platform/invite.rs:221` |
| 1604-1619 | 16 | Two owner-step explanation messages | `crates/control/lib/src/platform/install.rs:1567` |
| 2320-2329 | 10 | The HTTPS result count | The per-host loop at 2330-2362 |

Three counts in the file stay, because the count is the contract: one NATS (797), one invitation (1368), and one effect request (`release_documents.rs:488`).

### crates/control/lib/tests/support/release_documents.rs

The file has 1,164 lines today and 88 at the audit. Commit `3893d800c` (five tiers, one row per test target) added 1,057 of the new lines. It moved shared builders out of `environment_apply_kind.rs`, `support/http_connection.rs` and `support/integration.rs` into this file. That growth is sharing, not a copied fixture. The file holds fixture builders, and most of them assert nothing.

The cuts total 31 lines.

| Lines | Count | What it pins | Owner that keeps the contract |
| --- | ---: | --- | --- |
| 286-304 | 19 | `pin_web_clients`, which builds real web artifacts for a cluster with no web bucket | The web artifact tests named above |
| 572 (the `rows.len() == 4 &&` part), 573-575 | 3 | Fixture sizes (four rows, five bindings) | The per-handle target check at 576-593, `crates/control/lib/tests/environment_render.rs:573` |
| 1154-1162 | 9 | A repeated preload shape check | `crates/control/lib/tests/environment_render.rs:487` |

### crates/control/lib/tests/platform_render.rs

The file is new since the audit and has 1,777 lines. It is a contract-tier target (`tests/selection.toml:78`). Most of it is a set of test-side checkers. Each checker reads the rendered install documents and checks one relationship: service accounts, Secret and ConfigMap inputs, Role verbs, command inputs, Service selectors, script output, connections and package owners. Each checker came with tests that feed it a fabricated broken document and pin its exact diagnostic text.

The positive checks stay, because the renderer's unit test (`crates/control/lib/src/platform/render.rs:736-786`) checks only order and presence. The journey propagation tests at 1434-1777 stay, because they change an input and compare both consumers.

The cuts below remove the tests of the checkers, the fixture-count smoke test and one dead helper. They total 383 lines.

| Lines | Count | What it pins | Owner that keeps the contract |
| --- | ---: | --- | --- |
| 59-83 | 25 | `prekind_install_renders`: fixture counts and empty web maps | `crates/control/lib/src/platform/render.rs:736`, `crates/control/lib/src/environment/write.rs:261`, `crates/control/lib/tests/release_chart_render.rs:298` |
| 161-180 | 20 | A checker diagnostic for a missing service account | The positive test at 155 |
| 351-367 | 17 | A checker diagnostic for a missing Secret key | The positive test at 344 |
| 462-509 | 48 | Checker diagnostics for a missing verb and an unknown command | The positive test at 456 |
| 751-805 | 55 | Checker diagnostics for a missing input and an unbacked export | The positive test at 744 |
| 892-924 | 33 | Checker diagnostics for selectors | The positive test at 886 |
| 989-1043 | 55 | Checker diagnostics for discarded script output | The positive test at 983, `crates/control/lib/src/platform/invite.rs:221`, `:233` |
| 1240-1278 | 39 | `replace_job_document`, dead after the next two cuts | None needed |
| 1287-1310 | 24 | A checker diagnostic for a binding | The positive test at 1280 |
| 1367-1433 | 67 | Three package-owner cases that copy the same WMS setup | `crates/catalog/model/src/environment_application.rs:45`, `crates/control/lib/src/environment/analyse.rs:887`, `:896` |

A stricter reading of ruling 1 deletes the checkers too. Lines 84-1432 (1,349 lines) interpret rendered Kubernetes documents inside a test. Under that reading, the file goes to 428 lines. The renderer's own unit tests must then assert each relationship that the owner wants to keep. The owner decides.

### crates/platform/runtime/src/plugins/connection_http/tests/oauth2_tests.rs

The file has 1,946 lines today and 744 at the audit. The diff adds 1,235 lines and removes 33 (1,268 changed, 1,202 net). It is a module of the runtime library tests (`crates/platform/runtime/src/plugins/connection_http.rs:1673-1688`). That target is in live-nats (`tests/selection.toml:188`).

Most new blocks prove distinct token contracts: one locked refresh for two hosts (901-947), the shared 30-second failure window (949-1003), credential rotation (1005-1124), refresh-token retention (1126-1214), the authorization-code lifecycle (1271-1671) and token-origin confinement (1758-1946). They stay.

The cuts total 68 lines.

| Lines | Count | What it pins | Owner that keeps the contract |
| --- | ---: | --- | --- |
| 646 | 1 | The total request count, when the next assertions name both requests | The same test at 638 |
| 727, 734 | 2 | Internal cache sizes | The key checks of the same test at 705 |
| 1263 | 1 | A request total that 1262 already checks | `jwt_ps256_locked_fetch_and_401_retry` at 1516 |
| 1378-1382 | 5 | The no-row refusal, repeated | 1153-1157 |
| 1397-1403 | 7 | The decoded state size | `crates/platform/runtime/src/oauth_authorization.rs:434` |
| 1520-1556 | 37 | The ES256 and RS256 host-path copies and the malformed-key matrix | `crates/platform/runtime/src/plugins/connection_http/jwt/tests.rs:88`, `:139` |
| 1792-1806 | 15 | Two POSTs and two HEADs counted across two modes | 1778-1790, `jwt/tests.rs:271` |

The text assertions at 1410-1425, 1453-1460, 1584-1604, 1635-1659 and 1913-1916 stay for now. The callback error has no error kind today. It carries only `reason` (`crates/platform/runtime/src/oauth_authorization.rs:17-20`). A kind on that error is the fix, and that is a product change for the owner.

## Part B: the new ignore attributes

The audit counted 136 first-party ignores with a line-start match (`docs/audit/tests.md:276`). The same method gives 146 today. A match anywhere on a line gives 139 at the audit and 157 today. The 157 adds one documentation comment, one fixture string (`tests/conformance/tests/contract_diff.rs:273-277`) and nine parser inputs (`tests/conformance/tests/test_tiers.rs:464-481`). Those are not attributes. The real change is 20 added and 10 removed, so the net growth is 10, not 21.

Each of the 20 additions is a prerequisite. None is a bead quarantine or a disabled assertion. The table gives the action for each one.

| File:line | Test | Needs | Action |
| --- | --- | --- | --- |
| `crates/client/core/tests/generated_schema.rs:23` | `generated_update_schema_agrees_with_client_presence_and_null_behavior` | A generated schema file | Keep the test. Delete lines 56-59, which re-prove generated shape. |
| `crates/control/lib/src/platform/broker.rs:587` | `the_control_and_environment_users_open_their_memory_watches` | NATS | Keep |
| `crates/control/lib/src/platform/broker.rs:707` | `identity_makes_the_buckets_that_wamn_ctl_writes` | NATS | Keep |
| `crates/control/lib/tests/invite_live.rs:10` | `invite_refuses_an_org_with_an_active_admin_before_contacting_identity` | PostgreSQL | Delete the ignore and guard (4 lines) |
| `crates/control/lib/tests/release_chart_render.rs:231` | `web_only_changes_leave_all_workloads_byte_equal` | Helm | Keep. Helm is real, and this file is the chart's own test. |
| `crates/control/lib/tests/support/exclusive_resources.rs:274` | `exclusive_calls_wait_serialize_and_transfer_to_replacement` | PostgreSQL, NATS, Docker, guests | Keep |
| `crates/execution/host/tests/control_route_live.rs:1630` | `connection_routes_refuse_mismatches_and_project_only_enabled_rows` | NATS | Keep, and move the target to live-nats |
| `crates/execution/host/tests/control_route_live.rs:1921` | `delayed_grant_cannot_restore_revoked_admin` | NATS | Keep, and move the target to live-nats |
| `crates/execution/host/tests/host_route_live.rs:1044` | `cli_admin_revoke_reaches_the_same_http_refusal` | A built CLI | Keep |
| `crates/execution/host/tests/support/connection_test.rs:40` | `candidate_http_and_modbus_health_commit_nothing` | PostgreSQL, NATS, Docker, guests | Keep, and move the target to live-nats |
| `crates/execution/host/tests/support/oauth_authorization.rs:12` | `project_admin_authorize_and_public_pkce_callback` | NATS | Keep, and move the target to live-nats |
| `crates/identity/platform/tests/grant_authority_live.rs:39` | `inactive_membership_serializes_grants_and_refuses_sessions` | PostgreSQL | Delete the ignore and guard (4 lines) |
| `crates/identity/platform/tests/grant_authority_live.rs:143` | `service_admin_projects_without_user_membership_and_stale_admin_is_removed` | PostgreSQL | Delete the ignore and guard (4 lines) |
| `crates/identity/platform/tests/grant_authority_live.rs:234` | `authored_mutations_refuse_direct_admin_and_escalation` | PostgreSQL | Delete the ignore and guard (4 lines) |
| `crates/platform/runtime/src/plugins/route_authentication/tests/signature.rs:335` | `rotation_refuses_the_old_signature_on_the_first_call_after_the_delete` | PostgreSQL, NATS | Keep |
| `crates/platform/runtime/src/resource_lease/tests.rs:133` | `database_owners_unlock_and_recover_after_session_termination` | PostgreSQL | Delete the ignore and guard (4 lines) |
| `crates/platform/runtime/src/resource_lease/tests.rs:189` | `a_waiting_acquire_does_not_block_another_resource_release` | PostgreSQL | Delete the ignore and guard (4 lines) |
| `crates/platform/runtime/src/resource_lease/tests.rs:241` | `a_replacement_on_the_same_owner_waits_for_the_old_call` | PostgreSQL | Delete the ignore and guard (4 lines) |
| `services/host/src/host/lease_tests.rs:68` | `sigterm_drains_calls_then_hands_the_lease_to_a_ready_host` | PostgreSQL, NATS | Keep |
| `tests/integration/src/identity_session_test/membership.rs:169` | `membership_steps_write_as_provisioning_without_an_admin` | PostgreSQL | Delete the ignore and guard (4 lines) |

Eight ignores must go. Each one names only PostgreSQL, and each test sits in a target whose tier already supplies PostgreSQL through `wamn-test-postgres`. The owner ruled the same way on audit finding G2-5 (`docs/audit/tests.md:332`), and commit `7707712a3` removed those two ignores. Three of the eight are in the runtime library and one is in the integration library. So `cargo test -p` of those crates then needs the PostgreSQL 18 binaries, as the G2-5 tests already do. The command at `docs/operations/running-tests.md:197` loses its `--ignored` with them.

`crates/execution/host/tests/control_route_live.rs` is a live-postgres target (`tests/selection.toml:139`). Four of its cases need NATS, so the tier is wrong. The strongest resource sets the tier (`docs/operations/running-tests.md:361-369`). Move the target to live-nats. An ignore reason does not correct a wrong tier.

The ten removals since the audit are all right. Seven are the owner's Group 3 rulings at `7707712a3`. Two are the `cc773720c` ignore cleanup. One is a deleted test at `f4cbec305`.

## Part C: the practices behind the growth

Each practice below caused growth that Part A or B cuts.

1. Kind tests re-prove rendered shape. `bc6ba8627` added NATS authorization text to the kind case, and the edge image, account, selector and Role checks repeat `edge.rs:460`. These checks fail only at the end of a kind run, for defects that a unit test finds.
2. Progress and log prose are used as evidence. The step titles, skip messages, the invitation message (`eaf2f66b0`) and the qualification prose are change detectors. `3b599687a` shows the cost: an acceptance run failed on obsolete step titles and a removed Secret, and the fix only updated the expected list.
3. Tests of test checkers. Eight commits (`879bdd9bf`, `7f7d604d2`, `36f16245b`, `0c3552b93`, `0b237b6dc`, `eaf2f66b0`, `5864497f9`, `b2f110efd`) each added a checker of rendered documents plus fabricated-failure tests that pin its diagnostic text.
4. One contract at several tiers. `024f8ab9a` repeated the JWT algorithm matrix through the database host path, after `jwt/tests.rs` already owned it and `environment_apply_kind` covered ES256 at kind.
5. Ignores added without a tier check. Feature commits added ignored live regressions in targets whose tier already supplies PostgreSQL. The reason-format pass (`cc773720c`) fixed the syntax but did not check the tier.

Two further growths came from setup. `be8439e22` (this session's web split) added a real web artifact build to a kind case with no web bucket. `003eccd57` built resume orchestration into the kind test itself. A copied fixture caused little growth. The only copies are three `ConnectionFacts` constructions in `oauth2_tests.rs` (353-365, 383-396, 463-475) and the three package-owner cases in `platform_render.rs`.

The owner can adopt or strike each rule below on its own. Each rule prevents the practice with the same number.

1. Test a contract once, at the lowest tier that can observe it, and let a kind or live test assert only the state that its cluster or server alone shows.
2. Assert text, a count or a whole document only when that text, count or document is itself the contract.
3. Do not test a test helper: a checker of rendered output gets no fabricated-failure cases, and nobody pins its diagnostics.
4. Add an `#[ignore]` only for a prerequisite that the target's tier does not supply, and put a target in the tier of its strongest resource.
5. Answer a failed acceptance run with a product fix and the test that catches the defect, never with only an updated expected list, title or message.

## Part D: lines today and after the proposed cuts

| File | Lines today | Lines after the cuts |
| --- | ---: | ---: |
| `docs/history/evidence/sweeps/` (11 logs, Issue 1, done at `ce23920a2`) | 56,154 | 0 |
| `crates/control/lib/tests/platform_apply_kind.rs` | 2,819 | 2,313 |
| `crates/control/lib/tests/support/release_documents.rs` | 1,164 | 1,133 |
| `crates/control/lib/tests/platform_render.rs` | 1,777 | 1,394 (428 under the strict reading) |
| `crates/platform/runtime/src/plugins/connection_http/tests/oauth2_tests.rs` | 1,946 | 1,878 |
| `crates/control/lib/tests/invite_live.rs` | 92 | 88 |
| `crates/identity/platform/tests/grant_authority_live.rs` | 357 | 345 |
| `crates/platform/runtime/src/resource_lease/tests.rs` | 262 | 250 |
| `tests/integration/src/identity_session_test/membership.rs` | 236 | 232 |
| `crates/client/core/tests/generated_schema.rs` | 73 | 69 |
| Test files in total | 8,726 | 7,702 |

## Part E: the clone inventory

Issue `wamn-inmae.4`. The owner's standing rule of 2026-11-03 says that memory beats CPU on this platform, and that a clone is never free. This part lists the candidates. The owner rules on each non-mechanical candidate before anyone rewrites it.

### Method

The scan covers the production Rust of the three workspaces (`Cargo.toml`, `apps/Cargo.toml`, `apps/platform/no-std/Cargo.toml`) at `c6233b962`. It masks comments, string literals and character literals before it counts calls. It leaves out test modules, test-only features, fixtures, generated clients, benchmarks, examples, `vendor/` and `test-support/`. LOC counts nonblank, noncomment production lines. Each no-std crate includes the 50 lines of the shared runtime.

The columns are `.clone()` (C), `.to_owned()` (O), `.to_string()` on a `str` or `String` that the source confirms, out of all `.to_string()` calls (S/T), `.to_vec()` (V), `Arc::clone` (A) and `Rc::clone` (R). C includes calls on shared pointers. Density is 1,000 × C / LOC. The counts are written calls, without macro expansion, `.cloned()` or `Clone::clone`.

### Clones per thousand lines

| Crate | Root | LOC | C | O | S/T | V | A | R | C/KLOC |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| wamn-host | services/host | 2547 | 95 | 13 | 0/2 | 0 | 62 | 0 | 37.30 |
| rest-connector | apps/rest_connector/component | 217 | 8 | 2 | 0/5 | 0 | 0 | 0 | 36.87 |
| wms | apps/wamn_wms/component | 911 | 32 | 2 | 0/4 | 0 | 0 | 0 | 35.13 |
| wamn-router | crates/execution/workflow/router | 1067 | 33 | 0 | 12/13 | 0 | 0 | 0 | 30.93 |
| wamn-materializer | apps/platform/events/materializer | 443 | 13 | 3 | 1/3 | 0 | 0 | 0 | 29.35 |
| netsuite | apps/netsuite/component | 104 | 3 | 1 | 0/3 | 0 | 0 | 0 | 28.85 |
| wamn-workflow | crates/execution/workflow | 2093 | 60 | 16 | 0/3 | 0 | 9 | 0 | 28.67 |
| wamn-cdc-reader | services/cdc-reader | 1545 | 25 | 11 | 2/5 | 0 | 0 | 0 | 16.18 |
| wamn-scenario-worker | services/scenario-worker | 1550 | 25 | 10 | 1/4 | 0 | 2 | 0 | 16.13 |
| wamn-control | crates/control/lib | 44617 | 661 | 407 | 140/205 | 24 | 4 | 0 | 14.81 |
| materializer | apps/platform/execution/materializer | 652 | 9 | 2 | 2/5 | 0 | 0 | 0 | 13.80 |
| wamn-edge | services/edge | 3256 | 42 | 25 | 0/16 | 1 | 20 | 0 | 12.90 |
| wamn-execution-host | crates/execution/host | 6089 | 78 | 38 | 3/19 | 1 | 31 | 0 | 12.81 |
| jsonata-expression | apps/platform/execution/jsonata | 158 | 2 | 9 | 0/2 | 0 | 0 | 0 | 12.66 |
| blob-put | apps/platform/execution/blob-put | 174 | 2 | 0 | 5/5 | 1 | 0 | 0 | 11.49 |
| wamn-identity-client | crates/identity/client | 369 | 4 | 0 | 0/0 | 0 | 0 | 0 | 10.84 |
| http-page | apps/platform/no-std/http-page | 475 | 5 | 0 | 5/8 | 2 | 0 | 0 | 10.53 |
| http-route | apps/platform/ingress/http-route | 1784 | 17 | 9 | 20/31 | 2 | 0 | 0 | 9.53 |
| wamn-engine | crates/platform/engine | 5150 | 49 | 38 | 12/21 | 2 | 15 | 0 | 9.51 |
| wamn-catalog | crates/catalog/model | 8850 | 77 | 46 | 6/15 | 8 | 0 | 0 | 8.70 |
| wamn-run-state-sqlite | crates/execution/run-state-sqlite | 345 | 3 | 7 | 0/8 | 0 | 1 | 0 | 8.70 |
| wamn-schema-introspection | crates/schema/introspection | 4333 | 37 | 9 | 0/2 | 1 | 0 | 0 | 8.54 |
| wamn-ctl | services/ctl | 4146 | 35 | 12 | 1/10 | 1 | 0 | 0 | 8.44 |
| wamn-schema-control | crates/schema/control | 2662 | 22 | 2 | 62/63 | 0 | 0 | 0 | 8.26 |
| wamn-number-pattern | crates/platform/number-pattern | 366 | 3 | 0 | 0/0 | 0 | 0 | 0 | 8.20 |
| wamn-schema-generator | crates/schema/generator | 21916 | 176 | 311 | 3/15 | 12 | 0 | 0 | 8.03 |
| wamn-runtime | crates/platform/runtime | 23438 | 185 | 151 | 86/192 | 13 | 85 | 0 | 7.89 |
| wamn-session | crates/identity/session | 431 | 3 | 0 | 0/0 | 0 | 0 | 0 | 6.96 |
| client-acme-receiving | apps/client_acme_receiving/component | 745 | 5 | 11 | 0/4 | 0 | 0 | 0 | 6.71 |
| wamn-component-composer | crates/platform/component-composer | 759 | 5 | 0 | 0/0 | 2 | 0 | 0 | 6.59 |
| wamn-credential-store | crates/identity/credential-store | 308 | 2 | 6 | 0/2 | 0 | 0 | 0 | 6.49 |
| wamn-control-registry | crates/control/registry | 1088 | 7 | 0 | 3/4 | 0 | 0 | 0 | 6.43 |
| wamn-client | crates/client/core | 1353 | 8 | 24 | 4/11 | 2 | 0 | 0 | 5.91 |
| wamn-data-access | apps/platform/data/access | 355 | 2 | 2 | 0/7 | 0 | 0 | 0 | 5.63 |
| wamn-identity | services/identity | 2319 | 13 | 7 | 0/8 | 16 | 1 | 0 | 5.61 |
| wamn-execution-contract | apps/platform/execution/contract | 539 | 3 | 0 | 2/3 | 0 | 0 | 0 | 5.57 |
| wamn-package-artifact | crates/catalog/package-artifact | 839 | 4 | 16 | 0/0 | 1 | 0 | 0 | 4.77 |
| wamn-integration | apps/platform/wamn_integration/component | 238 | 1 | 1 | 0/1 | 0 | 0 | 0 | 4.20 |
| wamn-postgres-statements | apps/platform/data/postgres-statements | 560 | 2 | 1 | 5/5 | 0 | 0 | 0 | 3.57 |
| receiving | apps/wamn_receiving/component | 1150 | 4 | 5 | 1/12 | 0 | 0 | 0 | 3.48 |
| wamn-platform-identity | crates/identity/platform | 5307 | 14 | 25 | 0/8 | 3 | 0 | 0 | 2.64 |
| wamn-authoring-model | crates/authoring/model | 517 | 1 | 4 | 0/2 | 0 | 0 | 0 | 1.93 |
| wamn-control-provision | crates/control/provision | 4851 | 9 | 16 | 14/21 | 1 | 0 | 0 | 1.86 |
| wamn-run-state | crates/execution/run-state | 1986 | 2 | 1 | 7/9 | 0 | 0 | 0 | 1.01 |
| device | apps/edge_device/component | 57 | 0 | 2 | 0/0 | 0 | 0 | 0 | 0.00 |
| http-request | apps/platform/no-std/http-request | 294 | 0 | 0 | 4/6 | 2 | 0 | 0 | 0.00 |
| label-render | apps/platform/no-std/label-render | 275 | 0 | 0 | 6/6 | 0 | 0 | 0 | 0.00 |
| modbus-reader | apps/modbus_reader/component | 87 | 0 | 2 | 0/0 | 0 | 0 | 0 | 0.00 |
| samples | apps/edge_samples/component | 232 | 0 | 3 | 0/1 | 0 | 0 | 0 | 0.00 |
| transform | apps/platform/no-std/transform | 112 | 0 | 0 | 2/2 | 0 | 0 | 0 | 0.00 |
| wamn-component-policy | crates/platform/component-policy | 372 | 0 | 1 | 1/1 | 0 | 0 | 0 | 0.00 |
| wamn-component-virtualizer | crates/platform/component-virtualizer | 104 | 0 | 0 | 0/0 | 0 | 0 | 0 | 0.00 |
| wamn-event-reg | apps/platform/events/registration | 219 | 0 | 0 | 0/0 | 0 | 0 | 0 | 0.00 |
| wamn-event-wire | apps/platform/events/wire | 408 | 0 | 1 | 0/0 | 0 | 0 | 0 | 0.00 |
| wamn-modbus | apps/platform/no-std/modbus | 244 | 0 | 0 | 0/0 | 1 | 0 | 0 | 0.00 |
| wamn-pg-core | crates/platform/pg-core | 89 | 0 | 0 | 0/0 | 0 | 0 | 0 | 0.00 |
| wamn-project-state | crates/identity/project-state | 194 | 0 | 0 | 0/0 | 0 | 0 | 0 | 0.00 |
| wamn-record-history | apps/platform/data/record-history | 326 | 0 | 2 | 0/0 | 0 | 0 | 0 | 0.00 |
| wamn-submodule-check | tools/submodule-check | 191 | 0 | 5 | 0/0 | 0 | 0 | 0 | 0.00 |
| Total | Three workspaces | 165806 | 1786 | 1259 | 410/772 | 96 | 230 | 0 | 10.77 |

`wamn-control` holds 661 of the 1,786 calls. `wamn-host`, `wamn-workflow`, `wamn-router` and the WMS component have the highest density.

### crates/platform/runtime/src/plugins/wamn_postgres/operation.rs

The production part of the file (lines 1-803) has 22 `.clone()` calls on 19 lines. It also has seven `Arc::clone` calls and nine `.cloned()` calls. M means a mechanical fix inside the function. N means a fix that changes another function's signature or an owned field type.

| Line | Expression and type | How often | What a borrow, move or Arc needs | Class |
| ---: | --- | --- | --- | --- |
| 156 | `fact.statements.clone()`: `BTreeMap<String, ComponentSqlStatement>` | Per operation at workload binding | Lower the borrowed maps directly into the final owned map. | M |
| 743 | `facts.identity.clone()`: `SessionIdentity` | Per begin or read authorization | Build the kept fields explicitly, because the struct update replaces the others. | M, partial |
| 138 | `fact.registered_operation.clone()`: `Option<String>` | Per operation at binding | A manifest owner that outlives the workload, or shared field storage. | N |
| 739 | `facts.identity.schema.clone()`: `Option<String>` | Per begin without an operation schema | Owned or shared schema storage in the statements. | N |
| 128, 135, 138, 139, 141 | `id.clone()`: `String`, five calls | Per operation at binding | Map keys that borrow need an owner that outlives the workload. | N |
| 139 | `component.descriptor.component_digest.clone()`: `String` | Per operation at binding | A shared digest field type. | N |
| 141 | `schema.clone()`: `String` | Per operation with a runtime schema | A changed workload field. | N |
| 165 | `name.clone()`: `String` | Per named statement at binding | A changed member map type and a lifetime. | N |
| 259 | `path.clone()`: `String` | Per statement in each lowered set | A changed verified set type. | N |
| 261 | `statement.sql.clone()`: `String` | Per statement in each lowered set | A shared SQL field in `VerifiedStatement`. | N |
| 582 | `operation.clone()`: `String` | Per anonymous entry call | A changed `begin` signature or a returned context. | N |
| 721 | `operation.clone()`: `String` | Per anonymous begin | One field in `OpenOperation` in place of two. | N |
| 741 | `open.operation.clone()`: `String` | Per begin | Identity records that borrow need changed types and lifetimes. | N |
| 742 | `facts.applications[&open.operation].clone()`: `String` | Per begin | Owned or shared application storage in the statements. | N |
| 759, 760 | `delivery.clone()`: `String`, two calls | Per begin | A shared causation type. | N |
| 554 | `state(ctx.table)...clone()`: `StoreOperation` | Per entry call | This copies two Arc handles only. A borrow needs another state access design. | N |
| 594 | `trace.clone()`: `InvocationTrace` | Per entry call | A trace API that does not consume its receiver. | N |
| 394, 452, 530 | `Arc::clone(&store.workload)`: `Arc<str>` | Per read finish, rollback or commit | Move the handle into the callback after its last use. | M |
| 725 | `Arc::clone(&postgres)`: `Arc<WamnPostgres>` | Per begin | Move the handle into `open`, then use `open.postgres`. | M |
| 549, 564, 596 | `Arc::clone`: `Arc<str>`, `Arc<WamnPostgres>` | Per entry call | A changed context design or `begin` signature. | N |

`ComponentSqlStatement` owns its SQL, names, paths and two field vectors (`crates/catalog/model/src/component_facts.rs:266-287`). That is why line 156 is the costliest site in the file.

### The top twenty sites outside operation.rs

The rank follows the size of the cloned structure, not measured bytes. Whole documents come first, then nested collections and payload trees, then byte vectors.

| Rank | File:line | Expression and type | How often | What a borrow, move or Arc needs | Class |
| ---: | --- | --- | --- | --- | --- |
| 1 | `crates/control/lib/src/publish_release.rs:507` | `published.manifest.clone()`: `ServingManifest` | Per composed entry at publication | A publication API that consumes the manifest. | N |
| 2 | `crates/platform/runtime/src/local_application.rs:67` | `manifest.clone()`: `ServingManifest` | Per local application load | A changed loader signature, or a shared manifest owner. | N |
| 3 | `crates/platform/runtime/src/plugins/wamn_postgres/workflow_resolution.rs:358` | `local.manifest.clone()`: `ServingManifest` | Per local workflow resolution | Resolve inside the read closure and return the result. | M |
| 4 | `crates/catalog/model/src/serving_manifest.rs:1047` | `document.clone()`: release `Value` | Per release decode | Keep the comparison inputs, then move the document. Keep the refusal order. | M |
| 5 | `crates/catalog/model/src/serving_manifest.rs:1059` | `current.clone()`: release `Value` | Per release decode | Deserialize from `&current`. | M |
| 6 | `crates/control/lib/src/dev/coordinator.rs:182` | `package.manifest.clone()`: `PackageManifest` into a vector | Per package at catalog projection | A projection that takes borrowed manifests. | N |
| 7 | `crates/control/lib/src/environment/stage.rs:123` | `manifest.clone()`: `PackageManifest` into a vector | Per package at staging | Principal materialization that takes borrowed manifests. | N |
| 8 | `crates/execution/host/src/operation.rs:793` | `fact.statements.clone()`: `BTreeMap<String, ComponentSqlStatement>` | Per operation at member preparation | Lower each borrowed map directly. | M |
| 9 | `crates/control/lib/src/package_artifact.rs:930` | `facts.clone()`: `AdmittedComponent` | Per component at publication | A shared admitted fact, or a descriptor that borrows it. | N |
| 10 | `crates/execution/workflow/src/wiring_lowering.rs:188` | `resolved.document.clone()`: `WiringDocument` | Per wiring lowering | Lowering that borrows or consumes the document. | N |
| 11 | `crates/execution/workflow/router/src/walk.rs:415` | `a.payload.clone()`: payload `Value` | Per node call, retries too | A lifetime on `NodeCall`, or a shared payload. | N |
| 12 | `crates/execution/workflow/router/src/walk.rs:563` | `payload.clone()`: payload `Value` | Per outgoing edge | Shared payloads in frontier tokens. | N |
| 13 | `crates/execution/workflow/router/src/walk.rs:483` | `payload.clone()`: payload `Value` | Per successful node | Enqueue the successors first, then move the payload into `walk.result`. | M |
| 14 | `apps/platform/ingress/http-route/src/entry.rs:196` | `request_element.clone()`: request element `Value` | Per write element | A hash projection that skips excluded paths without a copy. | N |
| 15 | `crates/platform/engine/src/operation/intent.rs:189` | `call.input.clone()`: request `Value` | Per logged call with canonical form | Owned or mutable input in `OperationCall`. | N |
| 16 | `crates/platform/engine/src/operation/intent.rs:407` | `request_element.value.clone()`: element `Value` | Per new logged element | Another input form for the export call. | N |
| 17 | `crates/platform/engine/src/operation/intent.rs:455` | `body.clone()`: result `Value` | Per successful logged element | Move the body into `StoredOutcome`, borrow it for `finish`, move it back. | M |
| 18 | `crates/control/lib/src/web_artifact.rs:764` | `bytes.clone()`: `Vec<u8>` | Per uploaded web object | A shared byte buffer with a shallow handle for the compare. | M |
| 19 | `crates/control/lib/src/package_artifact.rs:427` | `data.clone()`: `Vec<u8>` | Per entry at package packing | A tar writer that borrows, or entries passed by value. | N |
| 20 | `crates/schema/generator/src/materialize.rs:509` | `source.bytes.clone()`: `Vec<u8>` | Per authored SQL file | Move `source.path` and `source.bytes`, because the loop owns `source`. | M |

### Sites that die under A5

None. A5 (`wamn-l5sus`) and its seven children are closed. Its plan keeps host delivery for workflows, wiring attachments and cross-application calls (`docs/plan/perf-arch.md:263-264`). `operation.rs` is the A5 replacement path: its workload binding takes a composed entry (lines 73-85), and it supplies `begin_operation` and `commit` (lines 469-533). So every site above stays, and a fix to it is not repeated work.

### Candidates for the owner

The mechanical sites (M) need no signature change outside the function. They are `operation.rs:156`, `:743` and the four Arc moves, and ranks 3, 4, 5, 8, 13, 17, 18 and 20. The non-mechanical sites (N) need an ownership decision: a lifetime, a shared owner or a changed signature. They wait for the owner's ruling.

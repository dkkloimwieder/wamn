# Session exchange: local and deployed proof, 2026-09-08

`wamn-ctc8.15.2` implements human PAT exchange through `POST /session` on `wamn-identity`.
A PAT is an existing personal access token.
The restored proof baseline is `bf24109d1339c61eca5ba8f0c32b526169c3fd10`.
The deployed session exchange also passed at source `6766a602`.
Git and Beads own landing status.

## Implemented scope

The service requires a current human PAT, exact project-environment membership, an active local user, and that environment's current tenant roles.
It refuses service PATs and unknown or mismatched targets.
The provisioner supplies each target's organization, project, environment, instance suffix, tenant, and database credential together in `target.json`.
The request supplies an audience, the intended project-environment, but no database authority.

The dedicated `SessionRoleReader` uses the existing A/B credential rotation and platform group.
Its explicit table grants contain only six SELECT columns: `users(tenant_id,id,status)` and `user_roles(tenant_id,user_id,role_name)` in `app_system`.
The identity service retains one connection per active target, created on first use, under the approved connection-lifetime ruling.
The issuer receives only the approved system identity and registry reads, alongside its existing signing-key authority.
The CLI recognizes the exact earlier issuer grants before upgrade and requires the exact expanded grants afterward.

The token carries the shared issuer, principal, organization, exact audience, and current environment roles.
Its expiry stays within 900 seconds of the original authentication start, even when authentication stalls.
This increment adds no refresh token, session database row, external identity provider, host token admission, or TUI login.

## Completed local proofs

The earlier passing runs used recorded working-tree changes based on `19aec3412c271576f7562c5a25a7478715321786`.
Their `source.base` and `source.patch` files identify the tested source, rather than a claim about an unchanged commit.
Each live run used its own disposable PostgreSQL 18 server.
Every completed live run cited here records readiness and cleanup exits of zero.
The [capture runner](ctc8-15-2-session/run-live.sh) retains commands, source snapshots, test output, exits, and available test and CLI binary hashes.

| Proof | Passed / failed | Raw output |
| --- | --- | --- |
| Provision library | 136 / 0 | [targeted-001/provision](ctc8-15-2-session/targeted-001/provision.log) |
| Exact target document | 8 / 0 | [targeted-001/target](ctc8-15-2-session/targeted-001/target.log) |
| Session request and response | 2 / 0 | [targeted-003/identity](ctc8-15-2-session/targeted-003/identity.log) |
| Issuer and reader CLI units | 3 / 0 and 1 / 0 | [issuer](ctc8-15-2-session/targeted-003/issuer-cli.log), [reader](ctc8-15-2-session/targeted-003/reader-cli.log) |
| Observer input and response rules | 3 / 0 | [targeted-003/observer](ctc8-15-2-session/targeted-003/observer.log) |
| Issuer grants and credential retirement | 1 / 0 | [live-issuer-001](ctc8-15-2-session/live-issuer-001/test.log) |
| Actual issuer CLI, including grant upgrade | 2 / 0 | [live-issuer-cli-001](ctc8-15-2-session/live-issuer-cli-001/test.log) |
| Actual target CLI and reader rotation | 2 / 0 | [live-audience-cli-001](ctc8-15-2-session/live-audience-cli-001/test.log) |
| HTTPS service and credential redaction | 2 / 0 | [live-surface-001](ctc8-15-2-session/live-surface-001/test.log) |
| Dedicated reader columns and both generations | 1 / 0 | [live-reader-004](ctc8-15-2-session/live-reader-004/test.log) |
| Fresh HTTPS session exchange | 1 / 0 | [live-exchange-002](ctc8-15-2-session/live-exchange-002/test.log) |
| Restored reader at `bf24109d` | 1 / 0 | [live-reader-006](ctc8-15-2-session/live-reader-006/test.log), [receipt](ctc8-15-2-session/live-reader-006/receipt) |
| Restored exchange at `bf24109d` | 1 / 0 | [live-exchange-006](ctc8-15-2-session/live-exchange-006/test.log), [receipt](ctc8-15-2-session/live-exchange-006/receipt) |

`targeted-002` also passed its identity and CLI tests.
Those repeated results do not add unique coverage.
The [targeted runner](ctc8-15-2-session/run-targeted.sh) also records the successful `wamn-ctl` build in `targeted-003`.

## Deliberate faults

Each deliberate fault changed production code based on `bf24109d`, compiled, and failed its intended runtime assertion.
All four test commands and overall runs exited 101, with one named failed test each.
The original source bytes were restored with `apply_patch` before the next fault.
Each run retains its exact change in `source.patch` and its executable hash in `test-binary.sha256`.

| Fault | Intended runtime failure | Raw output |
| --- | --- | --- |
| Bypass exact membership refusal | HTTP 200 instead of 401 | [live-exchange-003](ctc8-15-2-session/live-exchange-003/test.log) |
| Bypass the tenant predicate | `exact current tenant roles` | [live-exchange-004](ctc8-15-2-session/live-exchange-004/test.log) |
| Restart expiry at signing time | `expiry stays anchored before the authentication stall, not at resume/sign time` | [live-exchange-005](ctc8-15-2-session/live-exchange-005/test.log) |
| Grant the reader `users.email` | `dedicated reader must hold only the six approved SELECT columns` | [live-reader-005](ctc8-15-2-session/live-reader-005/test.log) |

The restored files match these baseline SHA-256 values:

| File | SHA-256 |
| --- | --- |
| `services/identity/src/session.rs` | `99e536695998a27fb95cc87053efef2ea6d73069db5c702ee99f02e167ec93cd` |
| `crates/control/provision/src/sql.rs` | `c19b278bbd264a637628af9ae51bb64ed5bfa7caa45a28f5722b16fc5215e7f7` |

Both restored runs passed their named tests with readiness, test, cleanup, and overall exits of zero.
Their source records identify `bf24109d` with no working-tree changes before or after execution.
The reader completed in 3.52 seconds and the exchange completed in 19.90 seconds, excluding compilation.

## Retained failures and limits

The earlier failures remain in the evidence.
Compilation failures did not execute a test and do not count as successful deliberate-fault proofs.
The first and third reader attempts compiled but failed during fixture setup.

| Attempt | Recorded cause | Raw output |
| --- | --- | --- |
| `targeted-001` identity | `AuthenticatedPrincipal` API mismatch, E0599 and E0308 | [identity log](ctc8-15-2-session/targeted-001/identity.log) |
| `live-reader-001` | The fixture selected an absent Unix socket | [test log](ctc8-15-2-session/live-reader-001/test.log) |
| `live-reader-002` | Unresolved `tokio_postgres` reference, E0433 | [test log](ctc8-15-2-session/live-reader-002/test.log) |
| `live-reader-003` | The fixture lacked schema access to `wamn_authority`, SQLSTATE 42501 | [test log](ctc8-15-2-session/live-reader-003/test.log) |
| `live-exchange-001` | Private `name` module access, E0603 | [test log](ctc8-15-2-session/live-exchange-001/test.log) |

The [first family matrix](ctc8-15-2-session/live-family-matrix-001/test.log) reported 17 passes and three failures.
Its new platform-group expectation omitted `wamn_session_role_reader`.
After that correction, the [second matrix](ctc8-15-2-session/live-family-matrix-002/test.log) reported 18 passes and two failures, with exit 101.
The dedicated reader row and the corrected platform-group row pass.

The remaining names are `the_guest_sql_family_is_refused_the_other_families_operations` and `the_management_admitter_family_is_refused_the_other_families_operations`.
The first expectation omits the existing `event_registrations` UPDATE-column grant.
The second expects six catalog SELECT grants that production removed.
`wamn-0h0g.15.137` records both failures on untouched `86b82b86c60ea73e74be5ac2da88e18809afbbf1` during `wamn-ctc8.19`, with 17 passes and two failures.

The original armed baseline logs were not located in retained evidence.
The [retained .19 workspace log](../../operations/evidence/ctc8-19-premerge-20260907/workspace-sweep-6c3b12f8.log) self-skips those rows and does not prove that baseline.
The baseline attribution therefore rests on the recorded issue and matching current assertions, not an exact comparison of original logs.

The [read-only summarizer](ctc8-15-2-session/summarize.sh) emits JSON for targeted and live increments and labels unfinished runs as incomplete.
Its counts include repeated test executions, not unique coverage.
These local proofs do not establish deployed behavior, host session-token admission, an integrated workspace pass, or completion of the identity epic.

## Static checks

The static run used `6a5fe24de27263f57fc2f80b7697f398f4c23d9a`, which merged main `509059f6` into the identity lane.
Its source records remained unchanged throughout the run.
The [overall exit](ctc8-15-2-session/static-001/exit) is 101, so the static suite did not pass as a whole.

| Subject | Result | Raw output |
| --- | --- | --- |
| Package architecture and workspace tiers | 9 / 0 and 9 / 0, exit 0 | [architecture](ctc8-15-2-session/static-001/architecture.log) |
| Identity Helm render with two session targets | Exit 0 | [render](ctc8-15-2-session/static-001/helm.log), [command](ctc8-15-2-session/static-001/helm.command) |
| Clippy for the five selected packages, all targets | Exit 0 with warnings | [Clippy](ctc8-15-2-session/static-001/clippy.log), [command](ctc8-15-2-session/static-001/clippy.command) |
| Initial platform inventory | 6 / 2, exit 101 | [platform](ctc8-15-2-session/static-001/platform.log) |
| Platform inventory after the argument-guard correction | 7 / 1, exit 101 | [chart-001](ctc8-15-2-session/chart-001/test.log) |

Clippy reported existing warnings and three new warnings that did not fail the command.
They are `format_collect` in `session.rs`, `manual_assert_eq` in `session_exchange.rs`, and `manual_string_new` in `session_target.rs`.
The equality assertion uses a boolean to avoid printing response values.
The [capture note](ctc8-15-2-session/static-001/helm-secret-list.note) records that the standalone Secret-list assertion did not run because `yq` was unavailable.

Commit `6766a6026230932c5ef61eabcb36f50f568c3a8c` changed only the test that reads the rendered identity arguments.
The rerun passes `the_identity_chart_requires_operator_inputs_and_renders_its_https_boundary`.
The remaining failure is `every_mounted_secret_is_declared_here_or_named_a_prerequisite`.
It names `wamn-object-store-credentials-acme--wms--dev` in `values-host-wms-pat.yaml`.

That diagnostic exactly matches the [earlier raw workspace log](ctc8-15-1-identity/integration-001/workspace.log), line 2558, at source `9f16600d`.
This comparison supports baseline attribution for the WMS Secret failure, but does not make the platform inventory pass.
These static results do not demonstrate deployed behavior.

## Deployed proof

The [journey verdict](ctc8-15-2-session/deployed-001/journey-verdict.json) records exit 0, `session_exchange_completed=true`, and `cleanup_failed=false`.
The run used clean source `6766a6026230932c5ef61eabcb36f50f568c3a8c`, a fresh kind cluster, disposable PostgreSQL 18, and a fixture CA.
The [commands](ctc8-15-2-session/deployed-001/commands.log) used the standard Dockerfile image targets and the actual identity Helm chart.
The actual provisioning CLI installed three target credentials and granted, then revoked, the human's exact environment membership.

The [initial Job](ctc8-15-2-session/deployed-001/identity-session-initial-verdict.json) passed four cases.
The [revocation Job](ctc8-15-2-session/deployed-001/identity-session-revoked-verdict.json) passed the next exchange refusal after the CLI revoked membership.
Both Jobs completed with one successful Pod each, container exit 0, and no restarts.
The [initial receipts](ctc8-15-2-session/deployed-001/identity-session-initial-receipts.txt) and [revocation receipt](ctc8-15-2-session/deployed-001/identity-session-revoked-receipts.txt) exactly match their expected records.

| Session case | Observed HTTP status |
| --- | --- |
| `human-dev` | 200 |
| `human-prod-no-membership` | 401 |
| `human-wrong-org` | 401 |
| `service-pat` | 401 |
| `membership-revoked` | 401 |

The successful case matched the expected principal, organization, audience, and `member` role.
The [public case records](ctc8-15-2-session/deployed-001/identity-session-initial-cases.json) retain those expectations without PAT values.
The observer Job received a read-only case Secret and the public fixture CA, but no database credentials.

Five public-key Jobs also passed the exact sets `{A}`, `{A,B}`, `{A,B}`, `{B}`, and `{}` through publication, activation, and removal.
Their receipts cover HTTPS configuration, issuer binding, unknown keys, wrong CA, plaintext refusal, and absent authority routes before session configuration.
The four nonempty stages also exercised two public-key caches.
All seven gate Jobs used the [built gates image](ctc8-15-2-session/deployed-001/images.json), matched its [loaded image identity](ctc8-15-2-session/deployed-001/gates-node-image.json), and recorded matching log hashes.

The [cleanup log](ctc8-15-2-session/deployed-001/cleanup.log) records deletion of `wamn-identity-jwks-6766a6026230-4101634` and both run-specific image tags.
The final capture-error log is empty.
This proves the named deployed session exchange, not host token admission, host verifier integration, or public-key cache expiry during an outage.

## First workspace attempt

The integration attempt used clean source `192db701412ca83c70df1823e863a70f892649b1`.
Its [Cargo preflight](ctc8-15-2-session/integration-001/cargo-preflight.log) found an existing Cargo process and two Rust compiler processes.
The preflight and [outer capture](ctc8-15-2-session/integration-001/exit) exited 1 before this attempt launched Cargo.
No workspace command, workspace log, or workspace exit was captured, and the source remained unchanged.
This attempt is not a failed workspace test run or a passing sweep.
The owner then directed removal of the machine-wide Cargo refusal.
The runner permits independent worktrees and keeps this lane's own commands serial, with its worktree-local target directory.
The earlier refusal remains recorded as an unrun attempt, not a repository-wide scheduling rule.

## Completed workspace command

`integration-002` ran at source `7424347dbd627c6bc15b414919dde88dfec5ffd1`.
Its [workspace command](ctc8-15-2-session/integration-002/workspace.command) finished with [exit 101](ctc8-15-2-session/integration-002/workspace.exit).
The [comparison](ctc8-15-2-session/integration-002/workspace-comparison.json) reports 1,908 passed tests and 65 failed tests across 169 targets, with 32 failed targets.
The 34 doctest targets report six passes and no failures.
The counts exclude one nested subprocess summary at log line 3364.

All 59 exact failed names from the [earlier baseline](ctc8-15-2-session/workspace-baseline.json) repeat.
The comparison contains six additions and no parser gaps.
The following line references identify their assertions in the [raw workspace log](ctc8-15-2-session/integration-002/workspace.log).

| Added failed test | Cause in this run | Log lines |
| --- | --- | --- |
| `the_platform_grain_family_set_is_pinned_and_not_derived` | This increment omitted `wamn_session_role_reader` from the expected platform-family array | 1133–1136 |
| `provision_project_env::tests::every_workload_family_carries_a_distinct_frozen_label` | This increment left 12 expected labels for 13 workload families | 1597–1600 |
| `dedicated_session_reader_columns_and_generations_execute_on_postgres` | Missing `WAMN_SESSION_ROLE_READER_PG_URL` | 1250–1251 |
| `compiled_cli_publishes_bound_session_targets_and_rotates_reader_generations` | Missing `WAMN_SESSION_AUDIENCE_CLI_PG_URL` | 1890–1891 |
| `session_exchange_uses_fresh_scoped_authority_without_session_state` | The owned disposable fixture was not armed | 2093–2094 |
| `an_exclusion_constraint_is_modelled_rather_than_refused` | The new main test lacks `WAMN_SCHEMA_INTROSPECTION_PG_URL` | 4147–4148 |

The two expected-array omissions belong to this increment, not the baseline.
The focused reruns below pass after their test-only corrections.
The other four additions lack explicit fixture inputs in this sweep, whose [environment record](ctc8-15-2-session/integration-002/environment) states that live variables were cleared.

Earlier armed proofs passed for the [dedicated reader](ctc8-15-2-session/live-reader-006/receipt), [audience CLI](ctc8-15-2-session/live-audience-cli-001/receipt), and [session exchange](ctc8-15-2-session/live-exchange-006/receipt).
Those separate results do not turn this unarmed workspace sweep into a pass.
The [contract-diff log](ctc8-15-2-session/integration-002/contract-diff.log) records 14 authoring passes, three `runtime-vendored-flow-http-routing` passes, and 16 `http-route` passes.
Its [command exit](ctc8-15-2-session/integration-002/contract-diff.exit) is zero.
The enclosing integration capture finished with [exit 101](ctc8-15-2-session/integration-002/exit) because the workspace tests failed.
Its [source-capture exit](ctc8-15-2-session/integration-002/source-capture.exit) is zero, and the regenerated comparison records `capture_complete=true` with no parser gaps.

## Verification checkpoint

`inventory-001` used source `7424347d` plus the [exact two-file test-only patch](ctc8-15-2-session/inventory-001/source.patch).
Commit `0f26546b` carries those corrections.
The source and patch remained unchanged during the focused reruns.
The [overall exit](ctc8-15-2-session/inventory-001/exit) and [source-capture exit](ctc8-15-2-session/inventory-001/source-capture.exit) are both zero.

| Corrected test | Result | Exact command and output |
| --- | --- | --- |
| `the_platform_grain_family_set_is_pinned_and_not_derived` | 1 passed, exit 0 | [command](ctc8-15-2-session/inventory-001/platform-grain.command), [log](ctc8-15-2-session/inventory-001/platform-grain.log) |
| `provision_project_env::tests::every_workload_family_carries_a_distinct_frozen_label` | 1 passed, exit 0 | [command](ctc8-15-2-session/inventory-001/frozen-label.command), [log](ctc8-15-2-session/inventory-001/frozen-label.log) |

These focused passes supersede the two inventory failures as evidence for those exact tests.
They do not replace the original workspace result or establish a full rerun after correction.
The `integration-002` sweep remains recorded as 1,908 passes, 65 failures, and exit 101.

The completed evidence covers local and deployed session exchange, scoped reader and CLI proofs, four deliberate faults, contract checks, and the focused inventory corrections.
It does not establish a passing integrated workspace suite.
Host session-token admission, host verifier integration, and TUI login remain outside this increment.

## Final integrated run

Main advanced with client, schema-generator, and shared-manifest changes during verification.
The lane merged main `e0d87e6d` and the inventory corrections into source `956b6a3bf6e45813143db9d6a703755b85fe478e`.
The final [workspace comparison](ctc8-15-2-session/integration-003/workspace-comparison.json) records 1,911 passes and 63 failures across 169 test targets.
The 34 doctest targets add six passes and no failures.
The [raw log](ctc8-15-2-session/integration-003/workspace.log) contains both corrected inventory tests as passes.

All 59 earlier failed names repeat, and the four additions are the same missing-fixture cases identified above.
The final comparison reports no parser gaps and no missing baseline failure names.
The separate armed reader, audience, and exchange runs remain their live evidence.
The broad workspace command remains red, with [exit 101](ctc8-15-2-session/integration-003/workspace.exit).

All three [contract-test groups](ctc8-15-2-session/integration-003/contract-diff.log) passed again, with 14, three, and 16 tests.
Their [exit](ctc8-15-2-session/integration-003/contract-diff.exit) is zero.
The source remained unchanged throughout the final run, with [source-capture exit zero](ctc8-15-2-session/integration-003/source-capture.exit).
This final run supersedes the earlier verification checkpoint without removing its failed attempts or raw evidence.

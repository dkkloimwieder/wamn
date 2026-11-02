# Generator edge, qualification cases, binding errors and command help

This report covers epic `wamn-68mv8`, which is audit group 2 and 3 part B (see [README](README.md)).
The branch is `worktree-genedge`, on main `229f7377e`.
Every pin below refers to that branch head, `9ef20278a`.

## Issues

| Issue | Audit reference | Commits | Result |
| --- | --- | --- | --- |
| `.1` Generator out of serving code | B1-1, B3-1 | `eaf6d5392`, `9ef20278a` | The package manifest, its declaration types, the release profile and the SQL lexer live in `crates/catalog/model`. Provisioning, schema control and the execution host have no generator dependency. |
| `.2` Declared qualification cases | B2-6, F3-5 | `d0fdb1eef` | Each package declares its cases in `tests/qualification.json`. `qualify-release` runs the union of the declarations of the release packages. |
| `.3` Infrastructure is not permission denial | A3-1 | `f0104c82a` | An unavailable principal database gives `ConnectionUnavailable`. A missing principal gives `PermissionDenied`. |
| `.4` Secret-free command help | E2-6 | `cdcca42e1` | Help hides the values of 36 secret-bearing environment inputs in `services/ctl`, `services/cdc-reader` and `services/scenario-worker`. |

After issue 1, `cargo tree -e normal` of `wamn-host`, `wamn-runtime` and `wamn-control-provision` shows no `wamn-schema-generator` and no `wamn-schema-introspection`.
A generator edit then does not rebuild the host image.
The host change rides the next platform build that a host change forces.

## Decisions

The owner gave one answer on 2026-11-02.

1. `entity_consumers` takes package and entity pairs. The callers in `crates/control/lib/src/dev/up.rs` and `test-support/infrastructure/examples/event_broker_files.rs` change one line each. The hard stops guard behavior, not mechanical signature changes.
2. The generator re-exports nothing. A re-export at the old path is a compatibility facade. Every user of a moved type imports it from `wamn_catalog`.

Root applied the same allowance to import-path edits in stopped files.
Issue 1 changed only import paths in `crates/control/lib/src/platform/packages.rs`, `test-support/infrastructure/operations.rs` and `test-support/infrastructure/postgres-run.rs`.
Issue 2 changed one call in `test-support/harness/src/delivery.rs` to `owned_command::execute`, because the qualification module that held its wrapper left `control/lib`.

The owner confirmed five root decisions on 2026-11-02.

1. The SQL lexer moves to `crates/catalog/model` with the release profile, because the profile needs it. It adds no external dependency.
2. The declaration file is `tests/qualification.json` beside the tests of each package. Each entry names the test `executable`, the exact case `name` and the `evidence_env` variable.
3. The case set of a release is the union of its package declarations, without repeats, in lexical order of executable, case name and variable.
4. A missing declaration refuses and names the package. A malformed one refuses and names the file and field. An empty list is valid for one package. A release with an empty union refuses, because a qualification that runs no case is no evidence.
5. `services/cdc-reader` stays in issue 4, because `alignment-f0` had no uncommitted work there.

Root decided these details inside the brief.

- The verb finds a package directory through the build owner's `build_record::source_root`, so no package list exists in code. `qualify-release` runs from a checkout.
- The Docker runner, the temporary results and the case checks move to `services/ctl/src/qualification.rs`. The payload, tags, push, lookup and registry verification stay in `crates/control/lib/src/delivery/qualification_artifact.rs`. `platform_apply_kind` runs the `wamn-ctl qualify-release` binary.
- Binding errors have two categories. An insufficient-privilege refusal of the principal read is authorization. A checkout, connection, prepare or query failure is infrastructure. The other binding steps make no database call and stay authorization.
- If the name of an input contains URL, PASSWORD, TOKEN, SECRET, KEY, PAT or CREDENTIAL, the input is secret-bearing. Names of file paths are not. The three existing PAT path hides on main stay. No argument was removed, because no mounted credential file supplies the same input.

## Regressions found and fixed on the branch

- Two packages compiled only through features that the removed generator edge turned on. `wamn-platform-identity` reads `serde_json::Value` through tokio-postgres without `with-serde_json-1`. `wamn-execution-host` uses `wash_runtime::washlet` without `washlet`, which also broke `wamn-workflow`. The workspace build and clippy passed. A check of each of the 45 packages alone found them, and `9ef20278a` declares both features. `wamn-bcplj` holds the check that keeps this class out.
- Main `229f7377e` added two uses of the old manifest path, in `ensure_project_database` and `install_sequence_live.rs`. `9ef20278a` moves them to `wamn_catalog`.

## Test results

- Generated bytes: `tools/lane-start` built all twelve packages on the branch and on main `4cbba6e89`. The package digests are identical. The build records differ only in one test import and in the generator source hash.
- The branch on `229f7377e`: fmt, workspace clippy and `wamn-ctl --features ops` clippy passed. The `platform_generation` tests passed.
- The unit tests passed in eleven packages: `wamn-catalog`, `wamn-control-provision`, `wamn-schema-control`, `wamn-execution-host`, `wamn-schema-generator` and `wamn-runtime`. They also passed in `wamn-control`, `wamn-cdc-reader`, `wamn-scenario-worker`, `wamn-ctl` and `wamn-host`.
- Ten `wamn-control` unit tests first failed on stale build records, because issue 2 adds `tests/qualification.json` to three packages. They passed after a rebuild. One test passed after `WAMN_IDENTITY_BINARY` named a built `wamn-identity`.
- Live targets of the changed files passed: `deploy_sql_authority` (17), `install_sequence_live` (2) and `release_qualification_live` (1).
- The new help test fails on main. It found `__WAMN_HELP_SECRET_WAMN_REPLICATION_PASSWORD__`.

The logs are in `/var/tmp/wamn-genedge-log`. The runs used no kind and built no platform image.

## New tests

These tests need tier rows from the tiers epic `wamn-kov6v`.

- `help_env_tests::help_hides_environment_values` in five binaries: `wamn-ctl`, `wamn`, `wamn-ctl-ops`, `wamn-cdc-reader` and `wamn-scenario-worker`. The shared source is `services/cli_help_tests.rs`.
- The declaration tests in `services/ctl/src/qualification/declarations.rs`, and the runner and case-check tests that moved from `control/lib` to `services/ctl/src/qualification.rs`.
- In `wamn-runtime`: `plugins::wamn_postgres::operation::tests::unavailable_principal_database_returns_connection_unavailable`, `missing_principal_returns_permission_denied` and `refused_statement_authority_returns_unknown_statement`.
- `captured_entities_get_consumers_without_registrations` in `wamn-control-provision` now takes pairs.

## Open items

- `wamn-bcplj` adds the check that compiles each workspace package alone. It changes the test selection files, so integration-schema takes it after `wamn-kov6v` lands.
- `qualify-release` now reads declarations from the checkout that runs it, and it no longer runs without one. The evidence payload records the case names that ran.

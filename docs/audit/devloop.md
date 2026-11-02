# Dev loop and build identity

This report covers epic `wamn-710me`, which is audit group 2 (see [README](README.md)).
The branch is `worktree-devloop`, on main `5129e3434`.
Part 2 of the cleanup stays unfiled.

## Issues

| Issue | Audit reference | Commits | Result |
| --- | --- | --- | --- |
| `.1` Build record rename | N1 | `6c1a32f49` | The rename is the first commit and it is alone. Serialized names follow it. |
| `.2` Saved edits reach the loop | F2-1 | `0a7d8d134`, `dd9677bfc`, `b5af3337d`, `76488d1c7` | The local apply takes the edited tree. |
| `.3` Watch covers the application | F2-2 | `0f6c71bbd`, `93ae4d0d6`, `ab885e14d`, `eadda33b8` | The watcher uses the build input paths. |
| `.4` Active application survives a failed candidate | F2-3 | `a947c6ae6` | A failed candidate leaves the application running. |
| `.5` Skip unchanged packages per cycle | DX 3.2 | `454b3eee5`, `ecaff318b` | Only changed packages and dependent overlays build. |
| `.6` Image identity and revision runner | H1-1, D2-8, H1-2, H1-9 | `d321130a1` | `tools/test-revision` replaces the old runner name. |

F2-4 (the fixed Receiving coordinate) is deferred to part 2 with `dev/environment.rs`.

## Decisions

Each stage keys on what it consumes, inside the one build record.
The schema stays `wamn.build/v1`.

- `inputs.digest` covers all build inputs. Admission and publication use it.
- `inputs.generation_digest` excludes `component/`, `data/`, and `web/`. Generate, ACL, and SQLx preparation use it.
- `build_record::schema_digest` also excludes `query/`. Migrate uses it. The build record does not store it.

The web build and client materialization moved from Generate to Build.

The loop stops the host before Migrate when it recreates the target or when a migration is pending.
All other changes stop the host at activation.

The owner retired `wamn-ri4b` ruling 5 for the dev loop on 2026-11-01.
The loop recreates the target only for an edited applied migration or for a history table with rows.
A stale `field_owners` edit keeps its data in development.
The baseline hash guards the deployed path.

## Regressions found by the saved-edit case

The local `local_delivery` case found these defects. It fails on each of them, so each failure has a local test.

- `wamn` panicked because it did not install a rustls provider. Commit `dd9677bfc` fixes this regression from `wamn-j1r05`.
- The loop connected to NATS without its certificate and failed with `UnknownIssuer`. Commit `b5af3337d` fixes this regression from `wamn-j1r05`.
- A named statement edit reran Migrate. Commit `eadda33b8` fixes this, and it adds unit tests.

## Test results

All results are for `aee656ce7`, the same source before the rebase over the `Chart.yaml` commit `5129e3434`.

- `local_delivery` passed in 806 s. The log is `/var/tmp/wamn-devloop/accept.main.log`.
- The workspace run passed 2,996 tests, failed 1, and ignored 139. The log is `/var/tmp/wamn-devloop/workspace.log`.
- The failed test is `install_job_tests::install_jobs_supply_required_environment`. It also fails on main, and its fix `1de1a97a4` is on `worktree-grants`.

The runs did not use kind, and they built no platform image.
The deferred part 1 runs stay as the first step of part 2.

## Open items

- The comment at `crates/control/lib/src/platform/web.rs:188` still says receipt. The amended scope allowed identifier changes only, so part 2 changes it.
- `crates/control/lib/src/dev/coordinator.rs` receives one added argument line from `wamn-ms1sn.2`. The branch that merges second rebases over it.

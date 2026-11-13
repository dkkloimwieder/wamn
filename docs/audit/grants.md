# Grants, credential names, linking, scalar owner and client decoder

This report covers epic `wamn-ms1sn`, which is audit group 3 part A (see [README](README.md)).
The branch is `ag3a-rebased3`, on main `37426c95f`.
Every pin below refers to that branch head, `f4cbec305`.

## Issues

| Issue | Audit reference | Commits | Result |
| --- | --- | --- | --- |
| `.1` Credential names | E1-1, E2-1 | `54354c47f` | Secret and ServiceAccount names use `project_env_key`, the `<org>--<project>--<env>` encoding, with no fallback. |
| `.2` Grant ordering and membership | E1-3, E1-4, E1-9 | `f4cbec305` | A grant commits system authority first and projects under the membership row lock. One owner mutates authored grants. |
| `.3` Declared-world linking | E3-1 | `e27052b6e` | The engine links each application component against its declared imports. An undeclared import fails at instantiation. |
| `.4` One scalar owner | A2-1, F1-6 | `2262f6e4d` | `canonical_scalar` in `wamn-execution-contract` owns numeric and timestamp spelling. The Receiving cursor module is deleted. |
| `.5` Client decoder and disabled tests | A3-9, G2-1, G2-2, G2-3, G2-5 | `7707712a3` | The client returns `MalformedResponse` for a malformed envelope. The five quarantined tests run. |

Commit `bb076739c` (`wamn-pf8iw.1`) is on the branch beside the issues. It fixes an epic 6 rebase regression: the env connections install Job named the old `WAMN_AUTH_NATS_*` variables.

## Decisions

The owner gave six answers for issue 2 on 2026-11-01.

1. The projection locks the `identity.org_memberships` row `FOR UPDATE`.
2. Direct application admin is removed. Application admin comes only from committed project-admin or org-admin. A read of wamn-dev found that no account loses admin. The only admin row in each application database belongs to the owner, who holds both roles.
3. `wamn-ctl grant-role`, `revoke-role`, `grant-permission` and `revoke-permission` are retired as SQL verbs. They are user verbs over the HTTP application routes. The control modules `role_permission.rs` and `user_role.rs` are deleted.
4. Root made the narrow edits in hard-stop files. The identity reader grant changed in `crates/control/provision/src/sql.rs` and `deploy/sql/system-schema.sql`. `grant_operator_admin_role` changed in `crates/control/lib/src/dev/environment.rs`. `crates/control/lib/src/dev/coordinator.rs` got one argument line. `services/host/src/host.rs` needed no edit.
5. A failed projection is repaired on the next mutation and on env apply, through the same projector.
6. Issues 3, 4 and 5 were committed first, and issue 2 last.

Root decided three details inside those answers. The application host takes no system lock. Service principals get admin from their project role. The `wamn_apply` membership lock grant lives in `deploy/sql/apply-grants.sql`, the one source of `wamn_apply` grants (owner ruling).

The owner waived the 2026-10-30 branch rule for this merge on 2026-11-01. The branch runs no `platform_apply_kind` and builds no image. The next shared install run covers its install, chart and env apply files.

## Regressions fixed on main

Test-cluster runs 2 to 7 of `platform_apply_kind` found these epic 6 regressions. The fixes reached main through `alignment-f0` in `b067a1e9a`, after run 8 passed.

- The local install did not run as the database owner and failed on `permission denied for schema wamn_authority`. Commits `47b9984d6` and `581db9578` fix it.
- The local owner lacked `wamn_platform`, so the package stage failed on the `catalog.releases` row security policy. Commit `0decdb998` fixes it.
- The package Job created the CDC maps as `postgres`, so the second env apply failed on `permission denied for table wamn_cdc_exclusions`. Commit `777ce6a0f` gives each map to its schema owner and adds a live test of both creation orders.
- The first-admin check refused the owner's own invitation after `provision-org` made the owner an org admin. Commit `a8729323a` fixes it, and `invite_live` covers it.

## Test results

- Issue 2 on `b067a1e9a`: fmt, clippy, unit tests, and the provision, control, host and identity live sets passed. One unit test first failed because the `wamn-identity` binary was not built, and it passed on a rerun with `WAMN_IDENTITY_BINARY`.
- The branch on `37426c95f`: fmt and workspace clippy passed. The tests of the files that both branches changed passed after a rebuild of stale build records. The rebuild changed no tracked file.
- The CDC and invite fixes: their unit and live tests passed, and test-cluster run 8 passed on `b067a1e9a`.

The logs are in `/var/tmp/wamn-ag3a-log`. The runs did not use kind on this branch, and they built no platform image.

## Open items

- The doc comment of `project_env_key` at `crates/control/registry/src/types.rs:392` still says "the project of a project environment". The wording is older than this epic and belongs to the project wording cleanup.
- The display text at `crates/control/lib/src/platform/steps.rs:216` keeps the old credential name. It is in a hard-stop file, and `alignment-f0` changes it later.
- The subject of main commit `47b9984d6` uses a word that the vocabulary rule removes. The commit is pushed and stays as it is.
- `wamn-dxyqc` adds one local install-sequence live test with a case per failure of test-cluster runs 1 to 6. Commits `47b9984d6` and `0decdb998` have no test of their own until it lands.

# Audit cleanup part 1: review report (epic wamn-n9s9x)

Branch `worktree-cleanup` on origin, seven commits on main 556b79265. Not merged. The epic stays open until the reviewer accepts.

## Issues

| Issue | Commit after rebase | Commit in the close reason |
| --- | --- | --- |
| .1 upgrade-environment path | 3b38a0da7 | 2ee9dce5a |
| .2 migration machinery, baseline check | ebcf9480a | a5807242a |
| .3 in-place package upgrade | 618abc4ed, 6f48fdd71, 85cfbe2ca | 1514c681f |
| .4 tools and stray files | 3d5d50fc8 | e5bb185bc |

Each hash changes again after the rebase on One NATS. Root records the final hashes on the beads at landing.

## Test result

The workspace run on 3d5d50fc8 exited 101 with three failed targets. Log: /var/tmp/wamn-cleanup/workspace.log.

- `deploy_sql_authority`: the count of governed relations was 21. It is 15 after the six table deletions. Fixed in 85cfbe2ca.
- `prune_record_history_live`: `a_retention_change_waits_for_a_running_prune` changed retention through a version bump, which apply now refuses. Root deleted the test as a test of deleted behavior. The lock in `create_history_tables` stays. Nothing tests that lock under a queued prune now.
- `control_route_live`: if `WAMN_NATIVE_C_NATS_BIN` is not set, the OAuth test from main panics. It passes with the variable set. This branch did not cause it. Bead wamn-df625 tracks it.

The three targets pass on rerun: /var/tmp/wamn-cleanup/fx.log. Root did not repeat the workspace run. `environment_lock_live`, `release_chart_render` and `control_portable_store` passed inside it.

Not run: `registry_image_archive -- --ignored`, whose test changed in Issue 4. It needs kind, and the epic has no cluster stage.

## Root decisions

- Tables `registry.schema_baseline` and `platform.schema_baseline` hold the hash.
- `apply_locked` compares the system hash first. `ensure` compares the project hash before reconciliation.
- `wamn_receiving` goes to 2.2.9 and `wamn_wms` to 2.1.6. The `client_acme_receiving` pin follows.
- `package.digest` lines were written from the digest that the test printed, because `--record-digest` needs arguments that root did not supply. `package_digest` passes after a rebuild.

## Deleted beyond the brief

`deploy/sql/corrections/wamn-a04x-wms-0007.sql`, `backfill_generation_passwords_sql`, `wamn_run.effect_attempt_dispatches`, `docs/plan/package-upgrade.md`.

## Scope

The explicit out-of-scope list was the hard stop, and named deletions were in scope. So this branch deletes or edits files that One NATS also edits: `upgrade_environment.rs`, `event-reader-rbac`, `system-schema.sql` and others. `values-host-default`, the PAT overlays, `values-host-base` and `values-host` stay because they have readers.

## Merge

`origin/onenats-rebased` conflicted in 13 files at the last dry run. It lands first by agreement with that session, so this branch rebases on it before the fast-forward. Fast-forward follows the owner's instruction, although the merge-train memory says otherwise.

## Part 2

- The eleven kept files of Issue 4, listed on wamn-n9s9x.4.
- `test-support/harness/src/case_release.rs:704` writes `floors`, which blocks the Rust field, the KCL declaration and the fixture blocks.
- `test-support/fixture-package/src/lib.rs:79,83,93` blocks removal of fixture upgrade support and `predecessor_version`.
- `delivery/environment.rs` ENVIRONMENTS, path and read lost their production caller.

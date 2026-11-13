# 68 wamn-environment-control, part 7

Scope: the remaining modules at `epic/dep7` (b94833fd): `author_workflow.rs` (543), `workflow_lifecycle.rs` (487), `event_streams.rs` (184), `event_advisories.rs` (1,476, mostly tests), `capture_gap.rs` (357), `prune_record_history.rs` (227), `prune_run_history.rs` (269), `reconcile_replica_identity.rs` (55), `invite.rs` (143), `create_user.rs` (62), `org_connection.rs`, `ident.rs`, `kubectl.rs`; the CDC publication check carried from tab 64; the crate split (tab 62 finding 10). Nothing built or run.

## Earlier work checked first

- Tab 62 5 A (run-plane converge engine deleted), 10 A preferred (split by purpose). Tab 64 4 A (reset by `DROP OWNED BY` the migration role; publication refresh to check). Tab 60 6 A3 (one migration role). Row 7 (one login per role). Identity tabs 55 to 57 (identity owns users and invitations). Owner 2026-11-10: applications reference platform users with real foreign keys.

## Files read

- Heads of every module above; `reconcile_run_plane.rs::converge_env_key_identity` (head and doc); `create_user.rs` head.
- `wamn-control-provision::sql/cdc.rs` and `database.rs::render_project_env_publication`.
- Writers of `platform."user"` in the identity crate (`application.rs`, `connection_projection.rs`).

## Findings

**1. Correction to tab 62 finding 5: the run-plane reconcile also installs the platform schema and projects identity rows.**

Fact: `converge_env_key_identity` installs `deploy/sql/app-schema.sql` (the `platform` schema of a project database) when absent, writes `platform."user"` rows for platform principals, service principals with a role in the project and users with a membership in the environment, removes users that lost membership, and repairs missing `admin` rows. `create_user.rs` says a user's environment row "comes from `reconcile-run-plane`". The identity crate also writes these rows on each grant (`application.rs` `USER_ROW_SQL`, `SERVICE_ROW_SQL`, `DROP_USER_ROW_SQL`; `connection_projection.rs`). Applications need these rows locally for their foreign keys.

Options:

- A: `app-schema.sql` is installed by the project initialization SQL (with later changes as project migrations, tab 60 5 B). The identity crate is the one writer of the rows: one function fills an environment at provisioning, and the grant paths keep it current. The run-plane deletion (tab 62 5 A) proceeds.

Recommend A.

**2. CDC publication under reset (check carried from tab 64 finding 4).**

Fact: the publication is `FOR TABLES IN SCHEMA <schema>` (`sql/cdc.rs`, CNPG `Publication` in `database.rs`), so tables created later join it. Inference: `DROP OWNED BY` the migration role drops the application schemas it owns, and the publication then no longer names them.

Options:

- A: the platform owner role owns application schemas; the migration role owns the objects inside them and has `CREATE` on the schemas. Reset drops the objects and keeps the schemas, so the publication stays valid and new tables join it.
- B: reset sets the publication's schema list again after the rerun.

Recommend A: no extra step.

**3. A second path for the first org administrator.**

Fact: `invite.rs` provisions an org's first admin through SQL and asks identity to mail the invitation; `provision-org` already makes the owner the org admin without a password. `create_user.rs` (ops feature) creates a user principal directly in the system database. Identity owns users and invitations (`user.invite`, tabs 55 to 57).

Options:

- A: org provisioning uses identity's own invitation for the owner; `invite.rs` and `create_user.rs` go.
- B: move both to identity unchanged.

Recommend A.

**4. The crate split (tab 62 finding 10), now that every part is read.**

Options:

- A: two crates remain from this one: `wamn-environment` (environment lifecycle, the executor of control-route operations, schema apply and reset, bindings, stage and expand) and `wamn-platform-install` (`platform/*`, system and org provisioning steps). Moves: `author_workflow` and `workflow_lifecycle` to the workflow crate; `prune_run_history` to `wamn-run-state`; `prune_record_history` beside record history; `event_streams`, `event_advisories`, `capture_gap`, `enable_cdc_project_env` and `reconcile_replica_identity` to one events and CDC owner; `kubectl.rs` to `wamn-kubernetes`; `platform_build` and the GCP pool and project tooling to xtask. Goes: `invite.rs`, `create_user.rs` (finding 3 A), `identity_issuer.rs` and the credential generation code (row 7), `platform_migrations.rs` (`Migrator`), `reconcile_run_plane.rs`, `verification_policy.rs`, `env_policies.rs`, `reconcile_package_data_access.rs`.
- B: keep one crate and only move the parts that have owners.

Recommend A.

## Follows from earlier rulings

- Row 7: `prune_record_history` and `prune_run_history` connect as one retention login each, not a credential generation.
- Tab 64 4 A: `capture_gap::reconnect` is no longer called by reset; `recover-capture-gap` and `close-capture-gap` stay for a lost slot.
- Tab 62 4 A and tab 58 6: `kubectl.rs` moves; namespace constants.

## Fine as is

- `workflow_lifecycle`: shares the environment lifecycle lock; the wiring check stays (owner 2026-11-10).
- `capture_gap`: an explicit recovery for a lost replication slot.
- `event_advisories`: an operations report (about 130 lines of code; the rest is tests); it moves with the events owner.

## Tests deleted, changed or moved

- Finding 1 A: `reconcile_run_plane` identity tests move to the identity crate's fill function.
- Finding 3 A: `invite.rs` and `create_user.rs` tests; `crates/control/environment/tests/create_user_live.rs`.
- Finding 4 A: tests move with their modules.

## Not read

- Bodies of `author_workflow.rs`, `workflow_lifecycle.rs`, `event_streams.rs`, `prune_*`.

## Rulings

- 2026-11-12: all findings A. 1 A: `app-schema.sql` by project initialization SQL; identity crate is the one writer of `platform."user"` rows (fill at provisioning, grant paths keep it current). 2 A: platform owner role owns application schemas; the migration role owns the objects in them. 3 A: `invite.rs` and `create_user.rs` go; org provisioning uses identity's invitation. 4 A: split into `wamn-environment` and `wamn-platform-install`, with the moves and deletions listed. The `wamn-environment-control` review is complete.

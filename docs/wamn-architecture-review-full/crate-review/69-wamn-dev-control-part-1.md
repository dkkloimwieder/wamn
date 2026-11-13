# 69 wamn-dev-control, part 1

Scope: `crates/control/dev` (`wamn-dev-control`) at `epic/dep7` (b94833fd): 17,714 source lines, 5,057 test and example lines. This part covers the ten-stage loop (`dev.rs`, `dev/plan.rs`, `dev/coordinator.rs`) and the development target database (`dev/target_database.rs`). Settles tab 52 finding 4 (dev introspection). Nothing built or run.

## Earlier work checked first

- Owner 2026-11-10: WASI virtualization of guest bytes goes (empty host context instead). Tab 53 output 7: the SQLx verifier, its dev stage and `sqlx-cli` pin go. Tab 52 finding 4 carried here.
- Tab 60 6 A3 (one migration role, `Migrator`); tab 64 3 A and 4 A (checksums, not fingerprints; reset by `DROP OWNED BY` and rerun). Tab 62 6 A (environment policy copy goes), 8 A (per-column grants go; default privileges). Tab 68 2 A (schemas owned by the platform owner role).

## Files read

- Module heads of every file in the crate.
- `dev/plan.rs` (`DEV_STAGE_ORDER`); `dev/coordinator.rs`: `migrate`, `introspect`, `prepare_generation`, `generate`, `build`, `virtualize`, `admit`, `gate`, `acl`, `reconcile_local_grants`, `run`, `stage_is_unchanged`, and every use of `self.catalogs`.
- `dev/target_database.rs`: header and function list.

## Findings

**1. Four of the ten stages do no work that survives earlier rulings.**

Fact (`DEV_STAGE_ORDER`: Migrate, Introspect, Generate, Build, Virtualize, Acl, Admit, Gate, Release, Activate):

- `generate()` only checks the `sqlx-cli` version and runs the SQLx verifier (`delivery::sqlx::prepare`). The generator itself runs earlier, in `prepare_generation`. Tab 53 output 7 deletes the verifier.
- `virtualize()` virtualizes nothing ("the Build stage virtualized each component"); it selects the built artifacts and verifies base digests. Owner 2026-11-10 deletes virtualization.
- `gate()` only checks that the number of admissions equals the number of artifacts.
- `acl()` converges per-column grants (`reconcile_package_data_access`) and replica identity; tab 62 8 A replaces the grants with default privileges.

Options:

- A: six stages: Migrate, Generate (the generator, now in `prepare_generation`), Build (with artifact selection and base digest checks), Admit, Release, Activate. Virtualize, Gate and Acl go; the admission count check moves into Admit; replica identity moves to Migrate.

Recommend A.

**2. The Introspect stage produces catalogs that nothing reads.**

Fact: `introspect()` calls `wamn_schema_generator::introspect_package` on the target for every package and stores the catalogs in `self.catalogs`. The only other uses are clearing them, recording them with the target (`record_target_schema`), restoring them (`retained_target`), and `stage_is_unchanged` counting them. No later stage reads them. `introspect_package` has no other product caller (tab 52).

Options:

- A: delete the Introspect stage, `record_target_schema`'s catalogs and `introspect_package` (with tab 52 finding 1's `materialize` cleanup). Settles tab 52 finding 4.

Recommend A.

**3. The development target is rebuilt by cloning a template database.**

Fact: `target_database.rs` (1,541 lines) leases a target, clones a stamped template database to recreate it (`replace_database`, `validate_template`, `template_fingerprint`), captures and restores the database ACL, records the schema inputs and catalogs, and shares the deployment schema decision (`schema_changes`, `adopt_schema`, `schema_operation`), which uses the fingerprints tab 64 removes.

Options:

- A: dev uses the same reset as an environment (tab 64 4 A): `DROP OWNED BY` the migration role and rerun migrations on the `Migrator`. The template, its fingerprint and validation, ACL capture and restore, recorded catalogs and `adopt_schema` go. The lease stays (one session per target).
- B: keep template cloning for speed.

Recommend A: one reset method for dev and environments.

## Follows from earlier rulings

- Tab 62 6 A: `migrate()` stops calling `project_environment_policy`. Tab 60 6 A3: `apply_local_package` runs on the `Migrator`.
- Tab 53 output 7: `sqlx_metadata_inputs`, `SqlxMetadataInputs` and the `cargo sqlx --version` check go. Tab 62 7 A: `claim_environment_instance` goes.
- Tab 68 1 A: `grant_operator_admin_role`'s note that `reconcile-run-plane` wrote the service row changes to the identity crate's fill.

## Tests deleted, changed or moved

- Finding 1 A: stage-order tests in `dev.rs` that name Virtualize, Gate and Acl; coordinator tests of those stages.
- Finding 2 A: Introspect stage tests; target schema record tests.
- Finding 3 A: template tests in `target_database.rs` (`local_lease_retains_data_and_reset_reapplies_exact_configuration` changes to the reset).

## Not read

- `release()`, `activate()` and the binding functions in `coordinator.rs` (part 2); `target_database.rs` bodies.

## Rulings

- 2026-11-12: all findings A (six stages; Introspect and `introspect_package` go, settling tab 52 finding 4; dev target reset by `DROP OWNED BY` and rerun, the template machinery goes, the lease stays).

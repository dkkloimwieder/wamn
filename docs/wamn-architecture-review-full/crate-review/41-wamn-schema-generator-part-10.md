# 41 wamn-schema-generator, part 10

Scope: data access (the column grants of the application database role). Nothing built or run.

## Files read

- `src/data_access.rs` (non-test part, in full).

## Callers read

- `crates/control/environment/src/reconcile_package_data_access.rs::read_presented_packages` and the calls to `derive_effective_data_access` and `render_effective_data_access_sql`.
- `generate.rs` (writes the file), `contracts.rs` (`generated_relation_access`), `materialize.rs` (parses the file), `build.rs` and `validation.rs` (`application_schemas`).

## One purpose per file

- `data_access.rs`: from a package manifest and the relation columns, derive which columns the application role may select, insert and update, and on which tables it may delete or lock; merge every installed package; render the GRANT and REVOKE SQL.

## Findings

**1. The grant file is shipped, then derived again to check it.**

Fact:

- The build writes `generated/platform-policy/data-access.json` (`DATA_ACCESS_OVERLAY_PATH`), and `PACKAGE_TAR_LAYERS` ships it.
- Reconcile (`read_presented_packages`) reads it, checks it is canonical compact JSON (`DataAccessOverlay::from_slice`), then derives it again from the package manifest and the file's own column lists and requires an exact match (`validate_data_access_contribution`). So the file holds nothing the manifest does not, except the column lists the build saw.
- Reconcile then merges the contributions with the live column lists (`derive_effective_data_access`) and refuses a contribution column absent from the live set (`EffectiveDesiredRelation::add`).
- Reconcile runs the migration plan check first (`plan_package_migrations`).
- The manifest bytes are parsed again for each call (`data_access_schemas`, `derive_data_access_overlay_from_relation_fields`).

Options:

- A: reconcile derives each package's contribution from its parsed manifest and the live column lists, with the one existing function. Delete the shipped file and its tar entry, `from_slice`, the canonical-bytes check, `validate_data_access_contribution`, `derive_data_access_overlay_from_relation_fields`, `data_access_schemas`, and the serde shape of the contribution. Inference: the drift check the file allowed (build-time columns against live columns) is the migration plan's job, which runs first.

**2. A third identifier rule.**

Fact: `valid_identifier` here allows a leading `_` and up to 63 bytes. `wamn_catalog::manifest::validate_identifier` requires a lowercase first letter, no trailing `_` and no `__`. Tab 36 finding 4 A made the catalog rule the one rule for table and column names.

Options:

- A: use the catalog rule. Delete `valid_identifier`.

## Fine as is

- `generated_relation_access` is the one definition of what a generated operation reads and writes; grants and operation contracts both read it.
- GRANT and REVOKE SQL is built as text: DDL with runtime identifiers, a named exception to U2 (tab 34).

## Tests deleted, changed or moved

- Finding 1: delete tests of `from_slice`, canonical bytes and `validate_data_access_contribution` (in `data_access.rs`, `validation.rs`, `reconcile_package_data_access.rs`, `feed.rs` reading the file). Reconcile tests derive from the manifest.
- Finding 2: identifier tests in `data_access.rs` go; the catalog rule keeps its own.

## Not read

- `data_access.rs` tests (names only); the rest of `reconcile_package_data_access.rs` (control tab).

## Rulings

- Findings 1–2: A (owner, 2026-11-11). The GRANT and REVOKE text stays as a named U2 exception, run with unchecked `sqlx::query`.

# 33 wamn-schema-generator, part 2

Scope: how a package build runs, end to end. Nothing built or run.

## Files read

- `src/build.rs`: `generate_package`, `pass_one_and_two` (in full); the function list.
- `src/materialize.rs`: `materialize_package*`, `introspect_*`, `generate_from_manifest`, `statement_corpus*`, `expected_files` (in full); the function list.
- `src/generate/validation.rs`: `validate`, `validate_lists`, `validate_custom_operation_sql`, `validate_constraint_error_mappings`, `validate_delete_target`, `derive_relations` (in full); the function list.

## Callers read

- Every product caller of the crate's build entry points: `services/ctl/src/build_verbs.rs` and `control/dev/src/source_deploy.rs` (`build_package`), `control/dev/src/dev/coordinator.rs` (`generate_package`, `introspect_package`).
- Every caller of the `materialize_*` family (crate tests, `tests/statement_check_live.rs`, two examples).

## Findings

**1. Two build paths.** Fact: the product builds through `build.rs` (`build_package`, `generate_package`). `materialize.rs` has a second path with its own two passes and its own file writing: `materialize_package`, `materialize_package_from_catalog`, `materialize_package_classified`, `materialize_package_verified`, `materialize_package_verified_with_catalog`, `classify_statements_with_existing_grants_in_transaction`, `materialize_after_introspection`, `statement_corpus`. Its only callers are tests. `materialize_package_from_catalog` writes every statement contract with `transactional = false`, output the product never makes.

- A: one build path. Delete the `materialize_package*` family and the helpers only it uses. Tests build through `build.rs`.

**2. Generation runs in full twice.** Fact: `statement_corpus_from_manifest` runs the whole of `generate` (every artifact, `rustfmt` on every `.rs`, client contracts) only to collect the `.sql` files and the grants. Then the statements are classified and `generate` runs again. The `transactional` bit never reaches the SQL bytes, so the corpus is the same both times.

- A: split SQL emission from contract emission. Emit the SQL corpus and grants once; classify; emit contracts once with the answers. Delete the discovery pass and `StatementTransactionality::unclassified` / `default`.

**3. Derived members are computed, written into JSON, then computed again and compared.** Fact, in one build process:

- Inheritance: `inheritance::complete_manifest` writes inherited fields into the manifest JSON; `generate` runs `inherit_model_fields` again on the result.
- Relations: `derive_manifest` derives them and writes them in; `validate` derives them again (`validate_static_sql_relation_access`) to compare. `derive_manifest` already refuses a manifest that states relations, so "a hand-written one states its own" cannot happen.
- Feed sources: the same, with `feed::derive` run in `derive_manifest` and again in `validate`.
- Enum value sets: tab 32 finding 1.
- `validate_operation_vocabulary` runs in `generate` and again first thing in `validate`.
- The manifest goes through five JSON forms (authored, typed, inherited, derived, canonical), each parsed again.
- A: one in-memory pipeline. Parse the authored manifest once; apply statement types, inheritance, relations and feed sources to the struct; write the compiled manifest once at the end. `generate` takes the completed struct and neither derives nor re-checks derived members. Detail of the JSON rewrite helpers in `authoring.rs` goes to the part that reads it.

**4. Authored SQL is read from disk three times.** Fact: `pass_one_and_two` loads it; `generate_from_manifest` loads it again for each of the two `generate` runs.

- A: load once, with 2 A and 3 A.

## Carried to later parts

- Two Postgres libraries in one build (from tab 32): `describe.rs` opens a SQLx connection for statement types and nullability; migration, introspection and classification open `tokio_postgres` connections. Options after `sqlx_metadata.rs` and `generate/rust.rs`.
- `output_root` layouts (from tab 32).

## Tests deleted, changed or moved

- Change (finding 1): the `materialize.rs` unit tests that call `materialize_package_from_catalog` (about ten), `tests/statement_check_live.rs` (`materialize_package_verified`, `classify_statements_with_existing_grants_in_transaction`) build through `build.rs`.
- Delete (finding 3): tests that hand a compiled manifest with stated relations or feed sources to `generate` and expect a mismatch refusal (to be listed with `validation.rs` tests).

## Not read

- The rest of `validation.rs` (model, operation, query, numbering, authored source checks) and `build.rs` (migration, history tables, statement types).
- `authoring.rs`, `sqlx_metadata.rs`, `generate/*` emitters.

## Rulings

- Findings 1–4: A.

# 52 wamn-schema-generator, part 20

Scope: the rest of `materialize.rs` and `build.rs` (entry points, passes, Postgres clients). Last file part of the generator. Nothing built or run.

## Files read

- `src/materialize.rs` (entry points, `client_bindings`, `generate_from_manifest`, `statement_corpus_from_manifest`, function list).
- `src/build.rs` (header, `build_package`, `current_build_inputs`, `generate_package`, `pass_one_and_two`, function list).
- `src/lib.rs` (exports); `Cargo.toml`.

## Callers read

- Every use of `materialize_package`, `materialize_package_verified`, `materialize_package_verified_with_catalog`, `materialize_package_from_catalog`, `materialize_package_classified`, `introspect_package`, `classify_statements_with_existing_grants_in_transaction`, `materialize_host_route_client`, `materialize_client_consumers`, `build_package`, `generate_package`, `current_build`, `current_generation`, `build_web_client` (grep).
- `control/dev/src/dev/coordinator.rs` (`introspect`, `prepare_generation`); `web/host-route-clients.mjs` (name only).

## One purpose per file

- `build.rs`: `wamn build` of one package: verification database, statement types, canonical manifest, consumers, build record.
- `materialize.rs`: today two jobs: the shared steps that `build.rs` calls (`generate_from_manifest`, `client_bindings`, `expected_files`, `write_files`, introspection, statement classification), and a second, older generation path that writes into the package root. Finding 1 leaves one job.

## Findings

**1. A second generation path with no product caller.**

Fact: `build::generate_package` is the product path (`wamn build`, dev, package artifact). The `materialize_package*` entry points (introspect a live database, generate, write the package's `generated/` directory) are called only by tests, test support and the `emit_client_ts` example. `materialize_package_classified` has no caller outside the file. `introspect_package` has one product caller (dev, finding 4).

Options:

- A: delete `materialize_package`, `materialize_package_verified`, `materialize_package_verified_with_catalog`, `materialize_package_from_catalog`, `materialize_package_classified`, `materialize_after_introspection`, `load_compiled_manifest` and their exports. Tests build through `build_package`. Rename what is left to what it does (the build's shared steps), or fold it into `build.rs`.

**2. The build runs the whole generator twice.**

Fact: `statement_corpus_from_manifest` runs `generate` (Rust, WIT, contracts, clients) only to collect the generated `.sql` files and the grant SQL. The build then asks PostgreSQL which statements need a transaction, and runs `generate` again to put that bit into the contracts. The source comment says both runs produce the same SQL.

Options:

- A: one function generates the SQL corpus and grants alone; the build classifies it, then runs the full generation once.

**3. Two PostgreSQL clients.**

Fact: `build.rs`, `materialize.rs`, `sql.rs` and `sqlx_metadata.rs` use `tokio-postgres`; the crate also uses SQLx (describe). U2 ruling: one methodology, SQLx.

Options:

- A: SQLx only; drop `tokio-postgres` from the generator. Introspection and classification run on a SQLx connection.

**4. Dev introspects the target database through the generator.**

Fact: dev's `introspect` stage calls `introspect_package` on the target database for each package and records the catalogs as the target schema. The build introspects its own verification database. Not read: what dev does with its catalogs after recording them.

Options:

- Wait for the dev tab. The question there: is the target catalog needed, or is the build's physical schema (`physical-schema.json`) the same fact?

**5. A product client is built by a cargo example driven by a Node script.**

Fact: the host-route TypeScript clients are written by `examples/materialize_host_route_client.rs`, which `web/host-route-clients.mjs` runs; `building.md` documents the cargo command.

Options:

- A: an `xtask` subcommand (tab 44); delete the example and the `.mjs` caller.

## Follows from earlier rulings

- `build_package`, `current_build`, `current_generation`, `current_build_inputs`: delete the freshness path (tab 42). `build_package` always generates and builds.
- `materialize_client_consumers` in `build_package`'s early return goes with it (tab 40: the client copy leaves the compile step).
- `generate_package` works on the authored manifest as untyped JSON: `with_placeholder_types` (a fake `text` type so the manifest parses), `apply_statement_types`, `complete_manifest`, `derive_manifest` returning bytes, `canonical_json_bytes`. The manifest is parsed four times. Typed derived fields and one serialize (tab 43 finding 1, tab 51 finding 3) delete the placeholder and the reparses.
- `load_base_manifests` reads base output without the build-record check (tab 42, tab 51).

## Tests deleted, changed or moved

- Finding 1: `tests/statement_check_live.rs`, `tests/support/platform_client_ir.rs`, `materialize.rs` unit tests on the deleted entry points, `test-support/fixture-package` change to `build_package`. Tests of deleted wrappers (`wrapper_and_split_paths_are_identical_without_reintrospection`, `stale_and_clean_materialization_produce_identical_outputs`) go.
- Follows (tab 42): `build.rs::current_build_preserves_files_and_changed_inputs_rebuild` goes.

## Not read

- `physical_schema`, `record_view_columns`, `missing_paged_query_indexes`, `migrate`, `create_history_tables`, `build_web_client` bodies.

## Rulings

- 2026-11-12: findings 1, 2, 3 and 5 A; finding 4 waits for the dev tab.

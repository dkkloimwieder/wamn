# 32 wamn-schema-generator, part 1

Scope: `crates/schema/generator` is 25,624 non-test lines in 39 files. It is read in about eight parts. Part 1: the crate root, the generation entry and the small helpers. Nothing built or run.

## Files read

- `src/lib.rs`, `src/generate.rs` (in full)
- `src/manifest.rs`, `src/output.rs`, `src/key.rs`, `src/rustfmt.rs`, `src/describe.rs`, `src/inheritance.rs`, `src/package_catalog.rs` (non-test parts, in full)
- `src/sqlx_metadata.rs` (head)

## Callers read

- Every outside use of `output_root`, `host_route_client_output_root`, `project_package_catalog`, `prepare_sqlx_metadata` (names and lines).
- Which files use `tokio_postgres` and which use `sqlx`.

## One purpose per file

- `lib.rs`: exports.
- `generate.rs`: `derive_manifest` (add derived members to the authored manifest) and `generate` (emit every artifact of a package), plus shared constraint and enum helpers.
- `manifest.rs`: one serde field spelling helper.
- `output.rs`: where a package's build output lies.
- `key.rs`: primary key type of a model.
- `rustfmt.rs`: format emitted Rust with `rustfmt`.
- `describe.rs`: statement parameter and column types from SQLx describe.
- `inheritance.rs`: overlay models inherit numbered and actor fields from their base.
- `package_catalog.rs`: cut the shared catalog down to one package and its bases.

## Findings

**1. CHECK value sets are read twice, from PostgreSQL's printed text.** Fact: `derive_manifest` runs `model_enum_fields` only to refuse bad lists and drops its result; `generate` runs `complete_enum_fields`, which reads the same CHECKs again with `check_value_set`. Both match the printed form `(col = ANY (ARRAY['a'::text, ...]))`. `guards_min_length` matches the printed form `(char_length(btrim(col)) >= n)`. The crate already depends on `pg_query`.

- A: one function returns the effective value sets; `derive_manifest` and `generate` both call it. CHECK expressions are parsed with `pg_query`, not matched as text.

**2. Constraint to error code, mapped twice.** Fact: `constraint_error` returns a `&str`; `constraint_error_code` returns `AccessOperationErrorLiteral` for the same three constraint types; `exclusion_violation` is a fourth literal written inline.

- A: one mapping to `AccessOperationErrorLiteral`; the string is its label.

## Carried to later parts

- Two ways to learn statement types from the database: `describe.rs` calls SQLx describe as a library; `sqlx_metadata.rs` stages a separate SQLx verifier crate and runs it (caller `control/release/src/delivery/sqlx.rs`). The crate also uses `tokio_postgres` (`sql.rs`, `build.rs`, `materialize.rs`). Options after `sqlx_metadata.rs`, `generate/rust.rs` and `build.rs` are read.
- Generation runs twice (`StatementTransactionality`): once to find the statements, again with the server's "needs a transaction" answers. Options after `build.rs`.
- `output_root` knows three layouts (`<package>/generated`, a build output directory, `<workspace>/target/wamn/<name>`), and logical `generated/...` paths differ from file paths. Options after `build.rs`, `materialize.rs` and the control callers.
- `inheritance.rs` uses `wamn_execution_contract::bytes_sha256`; tab 01 moves it to `wamn-digest`.

## Tests deleted, changed or moved

- Change (finding 1): tests of `check_value_set` and `model_enum_fields` call the one function.

## Not read

- `src/generate/*` (part 2 onward), the rest of the crate.

## Rulings

- Findings 1 and 2: A.
- Owner direction (2026-11-11): single responsibility, single source of truth, prefer deletion; destroy accidental complexity.

# 34 wamn-schema-generator, part 3

Scope: statement type checking (the SQLx verifier and SQLx describe). Nothing built or run.

## Files read

- `src/sqlx_metadata.rs` (non-test part, in full)
- `src/build.rs::record_view_columns`, the `view_column` use in `apply_type`
- `src/generate/contracts.rs::{emit_model, emit_custom_operation}` (in full); the function lists of `contracts.rs` and `rust.rs`

## Callers read

- `crates/control/release/src/delivery/sqlx.rs` and `sqlx/plan.rs` (`prepare`, `requires_verifier`, `require_cli_version`).
- `crates/control/dev/src/dev/coordinator.rs::generate` (the only product caller of `prepare`).
- `crates/control/dev/src/dev/watch.rs` (`.sqlx` path).
- Every reader of `generated/source-map/*.json`, `generated/native-verifier/*.rs` and `.sqlx` in the repository.

## One purpose per file

- `sqlx_metadata.rs`: stage a separate crate from the generated native rows and source maps, run `cargo sqlx prepare` on it, copy the `.sqlx` metadata into the build output.

## Findings

**1. Statement types are checked by two mechanisms, and neither covers everything.** (Rewritten 2026-11-11 after the owner's question. The first version said the build describes every statement; it describes only authored SQL.)

Fact:

- `describe.rs` calls SQLx's own `Executor::describe` (the machinery behind `sqlx::query!`) against the migrated build database. It writes no SQL. The build runs it only over authored SQL (`pass_one_and_two` describes the corpus from `load_authored_sql`) and uses the answer to fill and check the declared types of custom operations (`apply_statement_types`).
- The generator writes CRUD SQL by hand with `format!` (`sql.rs`: `get`, `create`, `update`, `delete`, `query`, `changes`). The statement contracts of that SQL take their column types from the catalog IR (`contracts.rs::operation_row_columns`), not from the database. The bind count is checked by lexing the SQL text (`sql_lex::parameter_count`).
- The SQLx verifier compiles `sqlx::query_file_as!` over every statement, generated and authored, against the native Rust rows. It is the only check that generated SQL returns what its contract says. But the native rows exist only for the verifier; the code that runs uses the `wamn` Rust version, read through `wamn:postgres`. The verifier runs only in `wamn dev` and needs `sqlx-cli` 0.9.0, a staged crate and a compile.
- Inference: SQLx's runtime and its macros cannot serve the guest. Guests reach the database only through the host's `wamn:postgres` interface; SQLx's macros produce code for SQLx's own drivers.

Options:

- A: one check, in the build, with SQLx describe over the whole corpus (authored and generated). Compare each statement contract (binds and columns, types and nullability) with what describe reports; the bind count comes from describe, not from lexing. Delete the verifier and everything that exists for it: `sqlx_metadata.rs`, release `delivery/sqlx.rs` and `plan.rs`, the dev SQLx step and `.sqlx` handling, the native Rust version (`Projection`, native rows, bind fixtures), the `source-map` and `native-verifier` outputs, `record_view_columns`.
- B: keep the verifier in release qualification only; remove it from `wamn dev`. The native Rust version and source maps stay; generated SQL stays unchecked in `wamn build`.
- C: drop `describe.rs` and read types from the verifier's `.sqlx`. Every build then needs `sqlx-cli`, a staged crate and a compile.
- Recommend A: one mechanism (SQLx describe), one place (the build), and it checks the contracts the running code uses.

## Carried item resolved

- Two Postgres libraries (tabs 32, 33). With 1 A, SQLx stays only as a library for `describe.rs`, which needs SQLx's nullability inference; `tokio_postgres` serves the rest. One job each.

Superseded (noted 2026-11-12): the line above was written before U2. U2 (Rulings, below) replaces `tokio_postgres` with SQLx in host and control code. Guests still use only the host interface.

## Tests deleted, changed or moved

- Delete (finding 1): `sqlx_metadata.rs` tests (including `verifier_carrier_changes_only_view_columns`); `build.rs::view_column_evidence_reaches_only_its_statement_accessor`; `release/.../delivery/sqlx.rs::package_search_path_preserves_other_connection_options` and the plan tests; the `.sqlx` case in `materialize.rs` tests; dev coordinator tests of the SQLx stage.
- Change (finding 1): tests that read `generated/source-map/*.json` or `native-verifier` files.
- Not read: `apps/wamn_receiving/tests/.sqlx` (an app test fixture); check in the apps tab whether its tests use SQLx macros.

## Not read

- `generate/rust.rs` and `contracts.rs` bodies beyond the two functions (next parts).

## Rulings

- Finding 1: A. SQLx describe checks every statement, generated and authored, in `wamn build`; the verifier and everything that exists for it are deleted.
- Owner ruling (2026-11-11): **U2, one build and runtime method for all SQL.**
  - Build: SQLx checks every statement, platform and application. Host code uses `query!` / `query_as!` with `.sqlx` offline data. Guest SQL is checked with SQLx describe and its typed accessors are generated from what describe reports.
  - Runtime: the host runs every query on SQLx (pool, transactions, `PgListener`); SQLx replaces `tokio_postgres` in host, runtime, control and services, and `rusqlite` on the edge. Guests reach the database only through the host interface; the host keeps credentials, caller settings, statement admission, per-operation transactions and the write log.
  - Known exceptions: SQL that builds identifiers at runtime (for example `references.rs` `DELETE FROM {relation}`, the view-dependency query in `control_route.rs`) cannot use `query!`; each is redesigned or named as an unchecked `sqlx::query`.
  - `wasmcloud:postgres` (wash-runtime built-in) has no transactions or sessions, so wamn keeps its own interface; whether its value types follow `wasmcloud:postgres` is checked in the runtime tab.
- Alternatives considered:
  - U1: SQLx in every component at runtime, guests included, over `wasi:sockets` (shown by wasmCloud, community meeting 2026-06-03). One coding pattern everywhere; moves credentials, caller settings, statement admission and transactions into the component that runs application code; per-instance pools; depends on Tokio's unstable WASI networking.
  - U1 for platform code only, with guests on the host interface (the U2 runtime split, but without a build-time check of guest SQL).
- Future work (owner request, not scheduled):
  - Benchmark: per-query cost of the host interface against SQLx over `wasi:sockets` in a guest, including result decode and connection use.
  - Developer ergonomics: how an application author writes, checks and calls SQL from a guest (generated accessors from describe today); revisit when SQLx on WASI networking is stable.
  - Benchmark (added 2026-11-11, tab 38 finding 2 C): the host's JSON-to-`Val` conversion and typed call, against the old JSON string plus guest parse. The claim that the costs are similar is an inference and must be measured before it is relied on.
  - Benchmark (added 2026-11-11, tab 39 finding 1 A): the host's `boon` input check per request, against generated checks in the guest.
  - Withdrawn (2026-11-11): the `boon` benchmark above. Tab 39 finding 1 changed to B, so the host does not check input.

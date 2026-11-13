# 04 `wamn-postgres-statements`

Path `apps/platform/data/postgres-statements`. One file, `src/lib.rs`: 685 non-test lines, 199 test lines. No workspace dependencies. It binds the WIT package `wit/wamn-postgres-0.4`, which was also read in full. Callers were searched in application source and in the generator, because most calls to this crate are in generated code. Nothing was built or run.

## One purpose

The Rust code an application component uses to run a statement the host has admitted, and to turn Rust values into SQL values and rows back into Rust values. (fact)

It is a guest library. Its users are the application components, `wamn-data-access`, and the code the generator writes.

## What it holds and who calls it

"Generator" means `crates/schema/generator/src` writes a call to it into generated code. "Apps" means hand-written source under `apps/`.

| Part | What it is | Callers |
| --- | --- | --- |
| `Connection`, `run`, `want_wal_position`, `wal_position` | Run one admitted statement by name | Generator and apps |
| `Transaction`, `begin`, `run_transaction`, `operation_transaction` | Transactions | `run_transaction`: 7 uses in apps. `operation_transaction`: generator and apps |
| `into_sql_value`, `IntoSqlValue`, `FromSqlValue`, `RowDecoder`, `decode_one`, `decode_optional`, `decode_all` | Value and row conversion | Generator |
| `Numeric`, `TimestampTz`, `Json`, `Uuid` | Four string wrappers with a public field and no check | Generator, `wamn-data-access`, apps |
| `StatementError`, `StatementErrorType`, `ContractMismatch`, `ContractPart`, `ValueShape` | Errors | Every application `error.rs` |
| `run_stream`, `RowStream` | Read rows as a stream, about 85 lines | None: not in the generator, not in any application source |
| `next_number`, `peek_number` | Platform number sequences | Generator; one app |

## What serves another purpose

| # | Finding | Mark |
| --- | --- | --- |
| 1 | `next_number` and `peek_number` are number sequences, not statements. The WIT has them inside the `statements` interface; its comment says they were added in place with no version change. | fact |
| 2 | `run_stream` and `RowStream` have no caller. `docs/architecture/data-access.md` says the host function stays for a workflow node that declares it and that no base read calls it. The host side is `runtime/src/plugins/wamn_postgres/run_stream.rs`, 373 lines. | fact |

## Where its definitions are repeated

| # | Finding | Mark |
| --- | --- | --- |
| 3 | The four scalar wrappers are defined a second time in `apps/platform/fixtures/sqlx-command/src/postgres.rs`, a 1,667-line second guest binding to the same WIT, with no dependents. | fact |
| 4 | A scalar value passes four layers. The check is in `wamn-execution-contract` (ruled to become `wamn-scalar`). `wamn-data-access/src/scalar.rs` wraps it and builds the wrapper type from this crate. Each application wraps that again in its own `data/scalar.rs` (`wamn_wms` 120 lines, `platform_fixture` 29, `edge_samples` 22). The wrapper type itself checks nothing, so `Numeric("abc".into())` compiles. | fact |
| 5 | The error list exists three times. The WIT defines `pg-error` and `statement-error`. This crate flattens them into `StatementErrorType`, 13 variants. Each application maps that again in `data/error.rs`: `wamn_receiving` names it 39 times, `client_acme_receiving` 25, `platform_fixture` 9. | fact |
| 6 | The WIT is bound here and again in the `lib.rs` of each application component (`wamn_receiving`, `wamn_wms`, `client_acme_receiving`, `edge_samples`, `platform_fixture`, `platform_fixture_overlay`, `rest_consumer_fixture`, `wamn_integration`). | fact. Not read: whether the second binding produces a second set of types. |

## Earlier rulings that meet this crate

Not decided again here.

- Dynamic read queries (Architecture tab): an application may run them. This crate has no function for one. The WIT has `client.query`, which takes SQL text; `component-policy` `APPLICATION_DENIED_INTERFACES` bars `wamn:postgres/client` for applications. The materializer and the sqlx fixture are the guests that use it.
- Shared application errors (Architecture tab, rules row 11): finding 5 is the code that ruling removes.
- `wamn-scalar` (tab 01): finding 4 is where it lands.

## Options

| # | Option | What goes | What is lost |
| --- | --- | --- | --- |
| 1 | Delete the guest stream wrapper | `run_stream`, `RowStream`, `READ_ROWS` | Nothing today. The host function and its 373 lines then have no guest caller; that is decided when the runtime is read. |
| 2 | One definition of each scalar type, with its check, in `wamn-scalar`. This crate imports it and only converts it to a SQL value. | The public-field wrappers here; `wamn-data-access/src/scalar.rs` as a middle layer for these four; the unchecked constructor | Nothing. A scalar that exists has passed its check. Not checked: how the generated code builds these values today. |
| 3 | Number sequences get their own interface in the WIT and their own small module | `next_number`, `peek_number` leave the statements interface | A WIT change that the host, the policy registry and every guest binding follow. The alternative is to leave them and accept two purposes here. |
| 4 | The fixture `sqlx-command` | Decided in its own tab. It is a crate in the list. |  |

### Every finding and its option

The owner asked why six findings had three options. Three findings had been passed on without one. They were read and now have options.

| Finding | Option |
| --- | --- |
| 1. Number sequences in the statements interface | Option 3 |
| 2. The stream wrapper has no caller | Option 1 |
| 3. Scalar types defined again in `sqlx-command` | Option 4, below |
| 4. A scalar passes four layers | Option 2 |
| 5. The error list exists three times | Option 5, below |
| 6. The WIT is bound twice per component | Option 6, below |

What the extra reading showed (fact):

- `sqlx-command` is used by one test, `crates/platform/runtime/tests/sqlx_transaction_live.rs`. It is an SQLx driver written over the raw-SQL `client` interface, to show that interface runs real transactions under row security. Its 1,667-line `postgres.rs` was not read line by line.
- Each application's `data/error.rs` declares its own `AccessErrorType`. In `apps/wamn_wms/component/src/data/error.rs` 11 of the 15 variants are platform errors that every application repeats (`InvalidInput`, `NotFound`, `ConcurrencyConflict`, `IdempotencyConflict`, `UniqueViolation`, `ForeignKeyViolation`, `CheckViolation`, `Retry`, `Timeout`, `PermissionDenied`, `InternalError`), each with its text literal, and 4 are the application's own. The mapping from `StatementError` is written again in each file.
- Each application `lib.rs` lists `wit/wamn-postgres-0.4` in its `wit_bindgen::generate!` path with `generate_all`. No hand-written application code names those local Postgres bindings; all of it uses this crate's types. So each component compiles a second, unused copy of the Postgres bindings. (fact for the search; inference that the copy is generated, since nothing was built)

| # | Option | What goes | What is lost |
| --- | --- | --- | --- |
| 4 | Decide `sqlx-command` with the raw-SQL `client` interface, in the runtime tab. If the `client` interface stays for dynamic read queries, one guest binding to it is needed and this fixture is the only one that exists. If it goes, the fixture and its test go. | Up to 1,667 lines and one live test | The only test that the raw-SQL interface commits, rolls back and obeys row security |
| 5 | The platform errors and their mapping from `StatementError` are defined once. An application declares only its own errors and adds them. This is rules row 11 of the Architecture tab, applied here. | The 11 repeated variants, their literals and the mapping in each application's `error.rs` | Nothing. Not decided here: whether the one definition sits in this crate or in `wamn-data-access`; row 11 chose the place. |
| 6 | This crate exposes its bindings, and each application maps the Postgres package to them in its `generate!` call. | The second copy of the Postgres bindings in every component | Nothing. Inference: `wit-bindgen` has a `with` setting for this. Not checked against version 0.44, which this repository pins. |

## Rulings (2026-11-11)

Owner ruling: options 1, 2, 3, 5 and 6 accepted. Option 4 needs more information; the owner asked whether a separate crate deals with SQLx.

| # | Ruling |
| --- | --- |
| 1 | Accepted. The guest stream wrapper is deleted. |
| 2 | Accepted. One definition of each scalar type, with its check, in `wamn-scalar`. |
| 3 | Accepted. Number sequences get their own WIT interface and module. |
| 5 | Accepted. Platform errors and their mapping are defined once. |
| 6 | Accepted. Applications reuse this crate's bindings. Carries a check: that `wit-bindgen` 0.44 supports it. |
| 4 | Open. See below. |

### SQLx in the repository

There is no separate SQLx crate. SQLx is used in three unrelated places. (fact)

| Where | What it does | Size |
| --- | --- | --- |
| `crates/schema/generator/src/sqlx_metadata.rs`, called through `crates/control/release/src/delivery/sqlx.rs` | Build time. It writes a throwaway Rust project and runs SQLx's own check of every admitted statement against the migrated database. | 492 and 111 lines |
| `apps/platform/fixtures/sqlx-command` | Run time, guest side. An SQLx driver written over the raw-SQL WIT interface `wamn:postgres/client`. | `postgres.rs` 1,308 non-test lines; `main.rs` 111 |
| `tests/conformance`, generator test support | SQLx as an ordinary PostgreSQL client in tests | not measured |

### What `sqlx-command` is (fact)

- `postgres.rs` implements SQLx's `Database`, `Connection`, `Row`, `Arguments`, `TransactionManager` and value traits for a type `WamnPostgres`. With it, guest code can write `sqlx::query(...)` and the host runs it.
- `main.rs` is one program: insert and commit, insert and roll back with no retry, and an insert that row security must refuse.
- One test runs it: `crates/platform/runtime/tests/sqlx_transaction_live.rs`.
- It is called private in `component-policy`. It is a fixture, not a library. No application can use it, because applications are denied the `client` interface.
- The only other guest that imports `client` is the materializer, which calls the WIT bindings directly with no SQLx.
- It defines its own `Numeric`, `TimestampTz`, `Json`, `Uuid`, its own error enum `WamnPgError`, and its own `run_transaction`. Each repeats a definition in `wamn-postgres-statements`.

Not read: `postgres.rs` line by line; whether another test drives commit, rollback and row security of the `client` interface from inside a guest. The plugin's own tests in `runtime/src/plugins/wamn_postgres/claims/tests.rs` cover them from the host side (from test names).

### Option 4, restated

|  | Option | What goes | What is lost |
| --- | --- | --- | --- |
| 4a | Delete the fixture and its test | About 1,420 non-test lines, its tests, the pinned `sqlx-core` entry in `apps/Cargo.toml` | The guest-side test that raw SQL commits, rolls back and obeys row security. An SQLx interface for guests that no guest uses. |
| 4b | Keep it as it is | Nothing | Three repeated definitions stay, against the single-source rule |
| 4c | Make the driver the library applications use for dynamic read queries | The fixture becomes product code | About 1,300 lines to own, and SQLx pinned at one version in every application that uses it. This is an addition, not a removal. |

The build-time check in the generator is separate and is not part of this option. It belongs to the generator's tab.

Owner ruling (2026-11-11): 4a. Remove it; it is trivial to rebuild if needed. What goes: `apps/platform/fixtures/sqlx-command`, `crates/platform/runtime/tests/sqlx_transaction_live.rs`, the `sqlx-command` entries in `tests/selection.toml` and `apps/Cargo.toml`, the pinned `sqlx-core` entry in `apps/Cargo.toml`, and the line in `docs/operations/running-tests.md`. Crate 04 is fully ruled.

## SQL

None. The crate passes statement names, not SQL text. (fact)

## Not read

- The test half of the file.
- The generated code under `apps/target/`, which is build output and was not built.
- The application components, beyond the searches named above.

## Tests deleted, changed or moved (added 2026-11-12)

Checked at `epic/dep7` (b94833fd) by grep. Test names are fact; which ruling each follows is from this tab.

Deleted:

- `crates/platform/runtime/tests/sqlx_transaction_live.rs`, with `apps/platform/fixtures/sqlx-command` (ruling 4).
- `runtime/src/plugins/wamn_postgres/claims/tests.rs::live_stream_reads_once_and_scopes_claims` and `an_unchecked_declared_statement_is_refused_before_a_stream_opens` (ruling 1; tab 10 ruling 4).
- `apps/platform/data/access/src/scalar.rs::uuids_timestamps_and_numerics_are_respelled` and `apps/wamn_wms/component/src/data/scalar.rs::uuids_and_timestamps_are_respelled_canonically`: the middle layer and the application copy (ruling 2). The checks they hold move to `wamn-scalar` tests.

Changed:

- Application `data/error.rs` tests (`client_acme_receiving` 2, `wamn_receiving` 3, `wamn_wms` 1): platform error cases move to the one definition; each application keeps tests of its own errors only (ruling 5).
- `runtime/src/plugins/wamn_postgres/numbering/tests/live.rs`: follows `next_number` and `peek_number` to their own interface (ruling 3).

Unchanged: the 6 tests in `postgres-statements/src/lib.rs`; the three quantity tests in `wamn_wms` `scalar.rs` (application validation).

Added: one live test that application raw SQL runs read-only (tab 10 ruling 3). It replaces the raw-SQL coverage of `sqlx_transaction_live.rs`.

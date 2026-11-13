# 36 wamn-schema-generator, part 5

Scope: the CRUD SQL writer. Read under tab 34's U2 ruling: SQLx describe checks every generated statement at build. Nothing built or run.

## Files read

- `src/sql.rs` (non-test part, in full)

## Callers read

- `crates/schema/introspection/src/ir.rs::postgres_type`; `src/describe.rs::column_type`; `runtime/.../wamn_postgres/statements.rs::postgres_types_match` (name).
- `wamn_catalog::manifest::{TombstoneColumn, validate_identifier}`.
- Every use of the literal `app.user_id` in product code (count).

## One purpose per file

- `sql.rs`: write the SQL text of `get`, `create`, `update`, `delete`, `query` and `changes` for a model, with `format!`.

## Findings

**1. Column types and PostgreSQL type names are mapped in three places, with three spellings.** Fact: `introspection/ir.rs::postgres_type` maps catalog spellings (`integer`, `timestamp with time zone`) to `ColumnType`; `generator/sql.rs::postgres_type` maps `ColumnType` to cast spellings (`int4`, `timestamptz`); `generator/describe.rs::column_type` maps SQLx's spellings (`INT4`, `TIMESTAMPTZ`, and `JSON` as well as `JSONB`). The runtime has a fourth comparison (`statements.rs::postgres_types_match`, not read).

- A: one table on `ColumnType` in `wamn-catalog`, keyed by the PostgreSQL type OID (introspection, SQLx and the runtime driver all report it), with the cast name for SQL text. Delete the three mappings.

**2. Tombstone column names are written twice.** Fact: `sql.rs` hard-codes `deleted_at` and `deleted_by` (`TOMBSTONE_MARKER`, `TOMBSTONE_ASSIGNMENT`, `LIVE_ROW`); `wamn_catalog::manifest::TombstoneColumn` owns the same names.

- A: `sql.rs` reads them from `TombstoneColumn`.

**3. The caller setting name is a literal in 65 places.** Fact: `app.user_id` appears 65 times in product code (generated SQL here, host, runtime, control, identity); `app.operation` and `app.env_key` likewise.

- A: one constant per setting in one crate (the crate that sets them for a transaction); generated SQL and every reader use it.

**4. Identifiers go into SQL text unquoted.** Fact: table and column names are written raw into every statement (`select_columns`, `returning_columns`, assignments, `'{history}'::regclass`). `validate_identifier` (lowercase snake case) is applied to manifest-declared relations; introspected column names come from the application's migrations (not verified that every one is checked). A name that needs quoting, such as a reserved word, would produce SQL that fails; with U2 the build's describe step catches it, at build.

- A: refuse, at introspection, any table or column name that needs quoting (one rule, the same as `validate_identifier`).
- B: quote every identifier the generator writes, through one helper.
- Recommend A (application names follow one rule; generated SQL stays readable).

## Tests deleted, changed or moved

- Change (finding 1): `sql.rs::key_casts_follow_the_primary_key_type`, introspection type tests, `describe.rs` type tests call the one table.
- Not affected: SQL snapshot tests keep their text.

## Not read

- `generate/contracts.rs`, `generate/rust.rs` bodies (next part).
- `runtime/.../statements.rs::postgres_types_match` body (runtime tab).

## Rulings

- Finding 4: A (introspection refuses table and column names that need quoting, with the `validate_identifier` rule).
- Findings 1–3: A.

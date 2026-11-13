# 50 wamn-schema-generator, part 18

Scope: how the platform reads SQL text: the feed derivation (`feed.rs`), the hand-written SQL lexer it and validation use (`wamn_catalog::sql_lex`), and the other SQL readers found on the way. Nothing built or run.

## Files read

- `src/feed.rs`: imports, helpers, function list (`derive`, `validate_view`, `view_from`, `missing_indexes`).
- `crates/catalog/model/src/sql_lex.rs`: header, `Token`, public function list; `sql_lex/grammar.rs` (function list; it is `#[cfg(test)]`).
- `crates/catalog/model/src/profile.rs` (header, `statements`).
- `crates/schema/introspection/src/migration_policy.rs` (its `Token` and scan functions, by name).

## Callers read

- Every `sql_lex::` use (grep): `generate/validation.rs` (relation access, delete targets, conflict constraints, filter predicates, group and window detection), `generate/contracts.rs` (`parameter_count`), `generate.rs` (`contains_schema_qualified_reference`), `feed.rs` (`groups_or_windows`), `profile.rs` (`profile_relation_access`).
- Every `pg_query::` use (grep): `feed.rs`, `control/package` (`declared_sql.rs`, `roles.rs`, `definition_ownership/plan.rs`), `control/environment` (`platform/identity.rs`, admin SQL tests), `schema/control/physical_schema.rs`.
- `crates/execution/host/src/control_route.rs` (calls `release_profile`).
- Guest crates under `apps/` that depend on `wamn-catalog` (none; two native test crates).

## One purpose per file

- `feed.rs`: derive the change-feed source of an authored query (its tables and join path) by parsing its SELECT and view with `pg_query`.
- `sql_lex.rs`: tokenize SQL by hand and derive facts from the tokens (columns read, written, locked; delete targets; conflict constraints; filter shapes; bind count).
- `profile.rs`: the effect profile of a release (per operation, the tables and columns its statements read, write or lock).

## Findings

**1. Three SQL readers: two hand-written lexers and the real PostgreSQL parser.**

Fact:

- `pg_query` (libpg\_query, the PostgreSQL parser itself) is already used in 7 product files across the generator, `control/package`, `control/environment` and `schema/control`.
- `wamn_catalog::sql_lex` is a hand-written tokenizer (1,583 lines, plus an 809-line test grammar) that derives relation access, delete targets, conflict constraints, filter shapes, group and window use and the bind count from tokens. Its header: "intentionally not a migration parser".
- `schema/introspection/src/migration_policy.rs` has a second hand-written scanner (block comments, single, double and dollar quotes) for migration policy.
- Inside the generator, `feed.rs` reads SQL with `pg_query` and `validation.rs` reads the same SQL with `sql_lex`.

Options:

- A: every SQL reading uses `pg_query` (parse trees, `scan`, `split_with_parser`). Relation access, delete targets, conflict constraints, filter shapes and group and window detection read the parse tree; migration statements are split and classified by `pg_query`. Delete `sql_lex.rs`, its test grammar, and the scanner in `migration_policy.rs`. The bind count already comes from SQLx describe (tab 34 finding 1 A).

**2. The host derives relation access again from SQL text.**

Fact: generation derives each custom operation's relation access from its SQL and writes it into the compiled manifest (`relations`, tab 43). The host's release profile (`control_route.rs` calls `wamn_catalog::profile::release_profile`) derives it again from the same SQL text with `sql_lex::profile_relation_access`. The profile's own header says they are "the relation access that generation derives for a custom operation, plus each view's base tables".

Options:

- A: the profile reads the manifest's derived `relations` (and the view base tables the build records); it parses no SQL. With finding 1, no SQL parser remains in `wamn-catalog` or the host.

## Fine as is

- `feed.rs` derives join paths from parse trees with `pg_query`.

## Tests deleted, changed or moved

- Finding 1: `sql_lex` tests and the test grammar go; the derivations get `pg_query`-based tests in the generator; `migration_policy.rs` scanner tests go, policy tests stay.
- Finding 2: profile tests read manifest relations.

## Not read

- `feed.rs` and `sql_lex.rs` bodies (structure only); `migration_policy.rs` policy rules.
- `generate/authored_feed.rs` (writes the authored feed functions as text; covered by tab 37 finding 5 A for its Rust).

## Rulings

- Findings 1–2: A (owner, 2026-11-11).

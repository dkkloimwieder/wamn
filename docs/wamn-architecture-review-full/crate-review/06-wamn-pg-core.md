# 06 `wamn-pg-core`

Path `crates/platform/pg-core`. One file, `src/lib.rs`, 241 lines. No dependencies at all. Ten crates depend on it. The file was read in full and every public symbol's callers were searched. Nothing was built or run.

Correction to the measured input (fact): `crates.json` gives 95 non-test lines. The test module sits in the middle of the file and code continues after it, so the counting rule stopped early. The real figure is about 190.

## One purpose

Quote a PostgreSQL identifier or string literal, and hold a name that has been checked to be usable as an identifier. (fact, from what callers use)

The file's own header describes a different purpose, which nothing uses. See finding 1.

## What it holds and who calls it

| Part | What it is | Callers outside the crate |
| --- | --- | --- |
| `quote_ident`, `quote_literal` | Two one-line quoting functions | About 87 import sites in 50 files: `control/environment`, `control/provision`, `control/package`, `control/dev`, `schema/control`, `platform/runtime`, `cdc-reader`, `ctl`, `identity` |
| `Identifier`, `InvalidIdentifier`, `QualifiedName` | A name checked for empty, NUL and the 63-byte limit | `control/dev` (12 uses), `schema/control`, a few others |
| `Sql` with `arity` and `param` | SQL text carried with its parameter count, so a caller can number added parameters | None. No file outside the crate names `wamn_pg_core::Sql`, and no code calls `.param(` or `.arity()` on one. |
| `AUTHORITY_SCHEMA`, `TENANT_KEY_FUNCTION`, `CURRENT_TENANT_KEY_FUNCTION` | The names `wamn_authority`, `wamn_authority.tenant_key`, `wamn_authority.current_tenant_key` | One file: `control/provision/src/tenant_key.rs` |

## Findings

| # | Finding | Mark |
| --- | --- | --- |
| 1 | `Sql` has no caller. The 50-line file header is about `Sql` only and says `wamn-run-state` uses it. `wamn-run-state` does not depend on this crate. The header says nothing about quoting, which is what every caller comes for. | fact |
| 2 | The quoting rule is written again in two crates that do not use this one. `schema/generator`: `build.rs::quote`, `data_access.rs::quote_identifier`, and twice inline in `materialize.rs`. `control/environment`: `reconcile_package_data_access/plan.rs::quote_identifier`. | fact |
| 3 | The three authority names are not a single source. Their one importer, `control/provision`, also writes the text `wamn_authority` 21 times in `sql.rs`. `services/scenario-worker` writes it 16 times. The comment on the constant says the schema compiler uses it; no schema compiler file imports it. | fact |
| 4 | The file is two files joined: `Sql` and its tests, then `use std::fmt` and the rest. The description in `Cargo.toml` says "guest-safe"; no guest depends on it. | fact |
| 5 | The name `pg-core` says nothing about quoting. | fact |

Carried finding, SQL scatter (tab 02): about 87 import sites of the quoting functions is also a count of places that build SQL text from parts at run time, mostly roles, schemas and grants. (fact for the count; inference that each builds SQL)

## Options

| # | For finding | Option | What goes | What is lost |
| --- | --- | --- | --- | --- |
| 1 | 1 | Delete `Sql`, its header text and its three tests | About 95 lines | Nothing |
| 2 | 2 | The generator and `control/environment` use this crate's `quote_ident` | Five private copies | Nothing. The generator gains a dependency on a crate with no dependencies. |
| 3 | 3 | The three names move to `control/provision`, which creates the schema and the functions, and `sql.rs` there uses them. The uses in `scenario-worker` are decided in its tab. | Three constants leave this crate; 21 literals in `provision/src/sql.rs` | Nothing |
| 4 | 4, 5 | After 1 and 3 the crate is quoting and checked names only, about 80 lines. Rename it to say so. A name is the owner's choice; `wamn-pg-quote` is one. | The name `wamn-pg-core` in ten `Cargo.toml` files | Nothing |

### Why it is a separate crate, and whether it can go

Owner question (2026-11-11): can this be folded into the other pg crate; why is it separate; is it just database utilities.

It is just database utilities: two one-line quoting functions and a checked name. (fact)

Why it is separate (fact, from its header and from who depends on it): it has no dependencies, so every crate that writes SQL text can use it, including the runtime and the CDC reader, without pulling in anything else.

No existing workspace crate can take it (fact, from the dependency data):

| Candidate | Why not |
| --- | --- |
| `wamn-postgres-statements` | A guest library built on `wit-bindgen` for application components. The users of the quoting functions are native platform crates. Platform pieces do not live with client pieces. |
| `wamn-test-postgres` | Test support. |
| `wamn-schema-control` | 3,820 lines, depends on the catalog. The runtime and the CDC reader do not depend on it and would have to, for two functions. |

But the two functions already exist outside the workspace (fact, checked this session): `postgres-protocol` 0.6.12 is in `Cargo.lock` as the wire library under `tokio-postgres`. Its `escape` module has `escape_identifier` ("escapes a SQL identifier and wraps it in double quotes") and `escape_literal` ("escapes a SQL literal and wraps it in single quotes").

| # | Option | What goes | What is lost |
| --- | --- | --- | --- |
| 5 | Delete the crate. Callers use `postgres_protocol::escape`. `Identifier` and `QualifiedName` move to `control/dev`, their main user, or go where the length check is not needed. The three names move to `control/provision` as in option 3. Option 2 then points the generator and `control/environment` at the library too. | The whole crate, 241 lines, and the five private copies | A crate with no dependencies. `control/provision` and `schema/control` do not name `tokio-postgres` today and would name `postgres-protocol` directly; it is already compiled into every binary that talks to the database. |

Not checked: the exact signatures; whether `escape_literal` writes a backslash differently from `quote_literal` (from general knowledge it switches to the `E'...'` form when the text has a backslash, which PostgreSQL reads as the same value); the dozen uses of `Identifier` in `control/dev`.

Option 5 replaces options 1 and 4. Options 2 and 3 stay, pointed at the library and at `control/provision`.

## Rulings (2026-11-11)

Owner ruling: accept removal. The crate `wamn-pg-core` is deleted.

| Item | Where it goes |
| --- | --- |
| `quote_ident`, `quote_literal` | Callers use `postgres_protocol::escape::escape_identifier` and `escape_literal` |
| The five private quoting copies in `schema/generator` and `control/environment` | The same library functions |
| `Identifier`, `InvalidIdentifier`, `QualifiedName` | `control/dev`, their main user |
| `AUTHORITY_SCHEMA`, `TENANT_KEY_FUNCTION`, `CURRENT_TENANT_KEY_FUNCTION` | `control/provision`, which creates those objects; its `sql.rs` uses them |
| `Sql` | Deleted. No caller. |

To check when the change is made: the library's signatures; that `escape_literal` gives PostgreSQL the same value for text with a backslash; the uses of `Identifier` in `schema/control` and other crates besides `control/dev`.

## Not read

- The 50 calling files, beyond the search.
- Why `scenario-worker` names `wamn_authority` by hand; six of its lines were read and they are privilege probes.

## Tests deleted, changed or moved (added 2026-11-12)

Checked at `epic/dep7` (b94833fd) by grep.

Deleted: the 4 tests in `crates/platform/pg-core/src/lib.rs` (`param_offsets_against_arity`, `a_larger_head_shifts_the_appended_params`, `text_and_arity_round_trip`, `quoting_and_identifier_validation_cover_adversarial_names`). Callers take quoting from `postgres-protocol`; a caller test keeps any adversarial-name case that its own code depends on.

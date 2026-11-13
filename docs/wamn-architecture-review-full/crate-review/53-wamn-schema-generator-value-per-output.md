# 53 wamn-schema-generator: value per output

Owner question (2026-11-12): the generator looks too complex and its value is unclear. This tab lists each output, who reads it, whether an existing tool does the job, and what is left after tabs 22–52. Nothing built or run.

## Size by area (fact, line counts of `src/`)

| Area | Files | Lines |
| --- | --- | --- |
| SQL, contracts and checks | `generate.rs`, `generate/contracts.rs`, `sql.rs`, `feed.rs`, `generate/authored_feed.rs`, `generate/numbering.rs`, `generate/routes.rs`, `generate/publication.rs`, `generate/validation.rs`, `data_access.rs` | 8,679 |
| Guest Rust | `generate/rust.rs`, `generate/data.rs`, `generate/component.rs` | 2,635 |
| WIT | `generate/wit.rs`, `generate/wit_adapters.rs` | 3,170 |
| TypeScript client | `client_ir.rs`, `client_plan.rs`, `client_ts.rs`, `client_fields.rs`, `client_route.rs`, `client_component.rs`, `route_schema.rs` | 6,462 |
| Build plumbing | `build.rs`, `materialize.rs`, `build_record.rs`, `components.rs`, `authoring.rs`, `output.rs`, `inheritance.rs`, `operation_reference.rs`, `sqlx_metadata.rs`, `cursor.rs`, `tofu.rs`, `describe.rs`, `key.rs`, `package_catalog.rs`, `rustfmt.rs`, `lib.rs`, `manifest.rs` | 8,096 |
| Total |  | 29,042 |

Tests: 14,652 lines.

## Product dependents (fact)

- Runtime (host, workflow, edge): only `build_record::check_current`. Tab 42 deleted it, so the runtime no longer depends on the generator.
- Control (dev, package, release, environment, provision) and `wamn-ctl`: mostly path helpers and the build record. Real uses: `build::generate_package`, `build_package`, `route_schema`, the data access overlay, `introspect_package`, `prepare_sqlx_metadata`, `render_project_tofu` (moves out, tab 49).

## Outputs

**1. SQL** (`generated/sql/**`, grant SQL, data access overlay).

- Reader: the guest (SQL is built into the generated data access), release, provision (grants).
- Value: high. This is the core job: model CRUD, queries by field, feeds, grants.
- Existing tool: none that writes SQL from this manifest.
- After rulings: one SQL reader (pg\_query, tab 50); one generation run (tab 52 finding 2).
- Options: A keep.

**2. Guest Rust** (`generated/data/*.rs`, `generated/component/*.rs`, operation modules).

- Reader: the application component build.
- What it does (fact): for each model action, `generated/data/{model}.rs` parses the checked input, calls `next_number` (a host import) for a numbered field, runs one generated statement through `wamn:postgres`, and maps the row or refusal. `generated/component/{model}.rs` exports it through the generated world. Actor fields, tombstones and history are already in the SQL and the `wamn_history` triggers, not in Rust.
- Call path today: host, then the application component (model export), then the host again (`wamn:postgres`), then the database.
- Existing tool: SQLx macros do not run in a guest (no SQLx driver over `wamn:postgres`). `quote!` and `prettyplease` replace the text writer (tab 37).
- Options:
  - A: keep, with tab 37 and 39 rulings.
  - B (inference): the host runs model CRUD itself. One host executor reads each model operation from the canonical manifest (statement, fields, types, refusals), checks and binds the input, calls `next_number`, runs the statement on its pool and maps the result. No guest call for model CRUD. Deletes `component.rs`, the model exports, model WIT and model codecs. Custom operations stay in the guest. Open: whether custom operations keep in-crate data functions (`data.rs`) for model access or call the host executor through a WIT import; where model input checks run (tab 39 finding 1 B put them in the generated function).

Validation for models today (fact): all declarative. Manifest: column type and nullability (introspected), `writable_fields`, `server_owned_fields`, `enum_fields` (also read from `CHECK (x = ANY (ARRAY[...]))`), per-create `values`, `min_lengths` (with a matching CHECK), `revision_field`, omission of a NOT NULL column with no default. Database: PRIMARY KEY, UNIQUE, FOREIGN KEY, CHECK, EXCLUDE map to declared refusals (`unique_violation`, `foreign_key_violation`, `check_violation`, `exclusion_violation`) through the model's constraints; triggers. No authored Rust runs in a model action; a rule in Rust means a custom operation in place of the generated action. Not read: how a trigger's `RAISE EXCEPTION` (P0001) maps. Under B the host runs the same declared checks; a Rust validation hook, if wanted, is a new feature under A or B (A: a call into an authored function in the crate; B: the host calls a component export before the statement).

**3. WIT** (`generated/wit/**`).

- Reader: `wit-bindgen` in the component build; the composer.
- Value: typed component calls (tab 38 finding 2 C).
- Existing tool: `wit-encoder` writes it; `wit-parser` checks it (tab 38). Codecs are deleted (tab 38).
- Options: A keep. Under 2 B, model WIT goes.

**4. Contracts** (`generated/contracts/**`).

- Reader: release `package_sources.rs`, package `component_admission`, `package_artifact.rs`, the client IR.
- Value: each operation's input, result, refusals, statement paths, transaction bit.
- Existing tool: none. Inference: most members are a projection of the canonical manifest; the build-only facts (transaction bit, statement paths) can live in the canonical manifest.
- After rulings: typed, one reader (tab 37); ten write-only members deleted.
- Options:
  - A: keep typed contract files (tab 37).
  - B: delete the contract files. The canonical manifest carries the build facts; one typed function in `wamn-catalog` gives an operation's contract from the manifest. One file fewer per operation, one fact source.

What each reader takes (fact): `component_admission.rs` reads `operation` and `statements` (path, binds, columns, `transactional`) and ignores the rest; `package_sources.rs` reads `operation`, `type`, `relations`, `record.revision_field`; the client IR reads input, result, errors and record. The host reads no contract file at run time. The one non-Rust reader is `tools/bench/pg-floor/run.sh` (operation id). Every member comes from the manifest, the catalog, the generated SQL or the build's plan check; `derive_manifest` already writes statement types and relations into the canonical manifest. With ruling C (overrides), an overridden action may run other statements than the default, so the statements an action uses must be declared where admission can read them, under A or B.

**5. Source maps** (`generated/source-map/*.json`).

- Reader: none in product code (fact: only generator code and generator tests read them). `wamn-catalog` refuses a custom operation whose name collides with a model's source-map path.
- Options:
  - A: delete the source maps and the collision refusal that exists only for their path.

**6. TypeScript client and browser components** (`client-ts/`).

- Reader: browser apps import the package.
- Value: typed calls, field maps (tab 47), generated components (`client_plan`).
- Existing tool: an OpenAPI document plus `openapi-typescript` would give types and calls; the component plan has no tool. Tab 47 ruled to keep the field maps and write no library.
- After rulings: one input shape (tab 48), typed contracts in (tab 45), no unused route data (tab 46).
- Options:
  - A: keep, with rulings.
  - B: the build writes an OpenAPI document from the route schemas; `openapi-typescript` writes the types; the generator keeps only the field maps and the component plan. Reverses part of tab 47.

Detail (fact, `client_ts.rs`): per operation the client holds camelCase request and result interfaces, enum and sort literal unions, request and result field maps, a route constant and one call function over an application-supplied transport; feed queries also get sync bindings for `@wfe/sync`. The web runtime classifies outcomes. No OpenAPI document exists in the repo. `openapi-typescript` would replace the interfaces only; it keeps wire (snake\_case) names, so field maps, camelCase (tab 47), sync bindings, outcome rules and components stay. B adds an OpenAPI emitter, result schemas and a Node step in the build. B's real gain is an OpenAPI document for consumers outside TypeScript.

**7. SQLx verifier** (`sqlx_metadata.rs`).

- Fact: dev writes a separate Rust crate with SQLx query macros over the generated SQL and runs `cargo sqlx prepare` (needs `sqlx-cli` 0.9.0 installed). The build already checks every statement against PostgreSQL twice: SQLx `describe` of authored statements, and `EXPLAIN (GENERIC_PLAN)` of all statements as the application role.
- Value (inference): it cross-checks the generator's Rust row types against the database. That is a test of the generator, run on every user's package.
- Options:
  - A: delete it from dev; a generator test runs it on the fixture packages.
  - B: delete it entirely; the two build checks stand.

Detail (fact, `sqlx_metadata.rs`, `dev/coordinator.rs`, `release/delivery/sqlx`): dev runs it for every package with a model operation or custom statement. It stages a crate under `target/wamn-sqlx-verifier/`, copies the repository `Cargo.lock` and appends a package entry, copies the generated SQL and `generated/native-verifier/*.rs` (a second row projection, `Projection::Native`, written only for this), and writes one `sqlx::query_file_as!` per statement with bind fixtures from the source maps. It runs `cargo sqlx prepare` against the target database and refuses unused queries. The `.sqlx` it writes has no reader. It checks the native row and bind types against PostgreSQL; the guest compiles the other projection (`Projection::Wamn`), so it checks a copy of the guest's types. Ejected or overridden code (output 2 C) is outside it.

**8. Build plumbing.**

- Most of it goes with earlier rulings: freshness and the build record checks (tab 42), the second generation path (tab 52), untyped JSON and reparses (tabs 43, 51), reference resolution per reader (tab 51), the test-only cursor (tab 51), the composer by `cargo run` (tab 40), tofu (tab 49).
- Left: one build function (verification database, describe, derive, generate, write), component build, the KCL call.

## Summary (inference)

- The core value is real and small: SQL, guest Rust, WIT, and the TypeScript client from one manifest.
- About 8,000 lines of plumbing exist to check, re-read and repeat that work; the rulings delete most of it.
- Options 2 B, 4 B and 6 B are the remaining large cuts. 5 A and 7 are plain deletions.

## Not read

- What each contract member's reader uses it for (tab 37 lists the write-only ones).
- Hooks, numbering and history in the generated CRUD code, in enough detail to cost 2 B.

## Rulings

- Owner direction (2026-11-12): generated CRUD is a starting point for the app developer, not a restrictive process. The developer must be able to customize or override it, as with the frontend. It is dev tooling.
- Output 2 ruled C (2026-11-12): the build generates each model action as a trait with default methods; the app overrides any method and may call the default inside it. A `wamn` dev command ejects one action's default code into the app's source for full ownership. The API shape (WIT, contracts, TS types), the default SQL and declared validation stay generated. Option B (host runs model CRUD) is withdrawn. The frontend follows the same pattern: wrap or replace works by composition today; the same eject command covers one component.
- Output 4 ruled B (2026-11-12): delete the contract files. The canonical `wamn.json` carries the build-only facts (statements with binds, columns and `transactional` per operation). One typed function in `wamn-catalog` gives each reader its contract view. An overridden or ejected action declares its statements in the manifest. Supersedes tab 37's typed contract files; the bench script reads `wamn.json`.
- Output 5 ruled delete (2026-11-12). Correction: the source maps have one product reader, the SQLx verifier, which output 7 deletes.
- Output 6 ruled A plus OpenAPI (2026-11-12): keep the Rust TypeScript emitter with the earlier rulings; the build also writes an OpenAPI 3.1 document from the same route schemas (input schemas exist; result schemas are added) for consumers outside TypeScript.
- Output 7 ruled B (2026-11-12): delete the SQLx verifier: `sqlx_metadata.rs`, `Projection::Native` and `generated/native-verifier`, the bind fixtures, `release/delivery/sqlx`, the dev SQLx stage and its state, the `sqlx-cli` pin, and the `.sqlx` output with its stale-file and watcher exceptions. Their tests go.

Frontend check (fact, `materialize.rs::copy_to_consumers`, `client_plan.rs` header): the generated client and browser components are copied into each consuming web package at `src/generated/{app}`, and that directory is rewritten in full (other files there are removed). A developer overrides by writing a component outside that directory that wraps or replaces the generated one; SolidJS composition gives wrap and replace with no generator support. No eject command exists. To match ruling C: a `wamn` eject command for one component, the same command as for a model action.

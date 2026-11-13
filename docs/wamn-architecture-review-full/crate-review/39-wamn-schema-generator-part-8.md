# 39 wamn-schema-generator, part 8

Scope: the generated data functions and handlers of CRUD operations. Read under tab 37 (`quote!`, utilities from existing crates) and tab 38 (finding 2 C: operations export only typed `run`; codecs deleted). Nothing built or run.

## Files read

- `src/generate/data.rs`: `request_type`, `parse`, `cursor_key_type`, `emit_model_data`, `statement_error`, `key_request`, `emit_get`, `emit_package_data`, `emit_sequences` (head), `emit_error` (in full). `emit_create`, `emit_update`, `emit_delete`, `emit_query`, `emit_feed`: structure only.
- `src/generate/component.rs`: header, `emit_model_component`, `handler` (in full).
- `src/generate/routes.rs` (what the route input schema states).
- `src/generate/wit.rs` validation emitters (`emit_mutation_validation`, `emit_crud_normalizer`, `emit_scalar_validation`, `emit_query_limit`), comments and checks.

## Callers read

- Every `mod generated` include site under `apps/*/component/src` (11).
- Uses of `generated::` items in authored app code (grep).
- `crates/platform/engine/src/flow_http_routing.rs::{InputSchemaValidators, compile_input_schema}`; `crates/client/core/src/request.rs::validate_schema`.
- Every `match` on `ColumnType` in the generator (grep).

## One purpose per file

- `generate/data.rs`: write one Rust data function per CRUD operation (parse the request values, run the statement, map statement failures to the package refusal), the refusal type, and the sequence functions.
- `generate/component.rs`: write one handler per operation (WIT request to data function arguments, data result to WIT result) and the generated world.

## Findings

**1. After tab 38 C and tab 31 finding 3, no layer checks the input.**

Fact:

- The generated codec checks each request: required members, refused nulls, value domains, the UUID spelling, declared minimum lengths (after a trim), the request count (1 to 100), `request_id`, and it fills the default limit (`wit.rs::emit_mutation_validation`, `emit_crud_normalizer`, `emit_scalar_validation`, `emit_query_limit`, `codec_prelude`). Tab 38 C deletes the codec.
- The host checks the published route input schema with `boon` (JSON Schema 2020-12) in `InputSchemaValidators`, today on the host path only. Tab 31 finding 3 deletes it. The client checks the same schema (`client/core/src/request.rs::validate_schema`, also `boon`).
- The route input schema (`generate/routes.rs`) states: required members, nullability, value domains (`enum`), the UUID pattern, `date-time`, the request count (`minItems`/`maxItems`), limit bounds, and `minLength: 1` on required text. It does not state the declared minimum lengths (with trim) or the default limit.
- Fact found on the way: the schema spells `bytes` as an array of integers 0 to 255. This confirms the tab 38 inference about bytes in JSON.

Options:

- A: the host checks every JSON input against the operation's published input schema (`boon`, kept from `InputSchemaValidators`, moved to the invoke path) before it converts to `Val`. One schema for client and host. The guest keeps only what the schema cannot say: spelling (the `scalar` parse it already does) and the default limit. Declared minimum lengths: the table CHECK already guards them (answer `check_violation`), or the schema gains `minLength` without trim. This partly reverses tab 31 finding 3 (the validator stays, in the invoke path).
- B: the generated operation function (finding 4) does every check, generated from the manifest.
- Recommend A.

**2. The refusal type is the same in every package.**

Fact: `emit_package_data` writes `generated/data/error.rs` from constants only (`CODES`, `ACTIONS`, `access_operation_error_detail`); no package input reaches it. Its own comment: "every package spells it the same way".

Options:

- A: one hand-written refusal type in `wamn-data-access`. Delete `emit_error`, `CODES`, `ACTIONS` and the generated file.

**3. A refusal travels as strings to reach the WIT error.**

Fact: the data `Error` gives `literal() -> &str` and `detail(key: &str) -> Option<String>`. A revision `i64` becomes a string there. The handler passes both to `codec::map_error`, which builds the WIT error variant by matching strings and parsing numbers back (`component.rs::REFUSE`; `wit_adapters.rs::emit_error_mapper`).

Options:

- A: one typed conversion from the refusal type to each operation's WIT error variant, a generated `match`. Delete `literal`, `detail`, `key_name` spellings and the carrier table in `emit_error`. (`map_error` already goes with the codec.)

**4. Two generated layers and two row types per operation.**

Fact:

- The data function (`generated/data/<model>.rs`) and the handler (`generated/component/<model>.rs`) are included in the same crate (each app's `component/src`). The handler only turns the WIT request into data function arguments and the data result back into WIT types.
- Rows exist twice: the statement row (`<Model>Row`, with `wamn_postgres_statements::Uuid` and other newtypes) and the WIT row. The `row!` macro (`wit_adapters.rs::emit_row_adapter`) copies one into the other, unwrapping `.0`.
- Authored product code calls no generated data function (grep); it reads `TIMESTAMPTZ_COLUMNS` and the sequence functions.

Options:

- A: one generated function per operation: it takes the WIT request, runs the statement accessor and returns the WIT result. The accessor decodes into the WIT row type (under tab 34 U2 its types come from describe). Delete the data function layer, the statement row types of CRUD operations and `row!`.

**5. Column type spellings are written in many places.**

Fact: separate `match` blocks on `ColumnType` give the Rust request type and the parser (`data.rs::request_type`, `parse`), the cursor key type (`cursor_key_type`), the WIT type (`wit.rs::wit_type`), the Rust row type (`rust.rs::projected_rust_type`), the JSON schema (`routes.rs`), the TypeScript type (`client_ts.rs`), client field types (`client_fields.rs`, two), and the PostgreSQL cast (`sql.rs`, tab 36).

Options:

- A: extend the one `ColumnType` table ruled in tab 36 finding 1 (`wamn-catalog`) to every spelling. Each emitter reads it. Delete the separate `match` blocks.

**6. Every app includes the generated code by hand.**

Fact: 11 include sites under `apps/*/component/src` each write the path (`../../target/wamn/<package>/...`) and their own list of `#[expect(...)]` lints.

Options:

- A: one macro in `wamn-data-access` that writes the module, the path and the one lint allow (tab 37 finding 5). Delete the per-app copies.

## Tests deleted, changed or moved

- Finding 1: tests of the codec checks move to host input checks (one schema test per check type, at the host tier). Tab 31's `InputSchemaValidators` tests stay, moved with it.
- Finding 2: the refusal type gets unit tests in `wamn-data-access`; generated `error.rs` snapshots go.
- Findings 3–6: generator snapshot tests change; app tests that build `data::generated::*` rows or call `codec::decode` change (`wamn_receiving/component/src/reads.rs`, `codecs.rs`, `client_acme_receiving/component/src/codecs.rs`).

## Not read

- `data.rs` bodies of create, update, delete, query and feed (structure only); `emit_sequences` body.
- `components.rs`, `client_component.rs`, `data_access.rs` (next parts).

## Rulings

- Finding 1: **B** (owner, 2026-11-11; first ruled A, then changed the same day). Every input check runs in the generated operation function (finding 4), compiled into the component, with helpers from `wamn-data-access`. The checks and the published input schema are both generated from the same manifest declaration in one run. The host only converts JSON to `Val`; a value that does not fit the WIT type is refused there.
  - Why B: typed calls between components do not pass the host, and one of them is an overlay (a client's package) calling a base package operation (`client_acme_receiving` calling `base::run`). B checks every caller the same way, and the checks are compiled, not interpreted at run time.
  - Costs accepted: the generated check code stays (built with `quote!`, tab 37); bad JSON costs one guest call before refusal.
  - Client: still checks the published schema for early feedback.
  - Tab 31 finding 3 stands as ruled: `InputSchemaValidators` is deleted.
  - Alternative considered: A, the host checks the published schema with `boon` (one generic validator; typed calls between components unchecked).
  - Correction to the first comparison: B is not two sources of the rules; both outputs come from one manifest declaration. Only the generator has two emitters.
- Findings 2–6: A (owner, 2026-11-11).

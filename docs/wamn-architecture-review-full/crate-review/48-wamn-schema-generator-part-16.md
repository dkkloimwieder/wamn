# 48 wamn-schema-generator, part 16

Scope: the rest of the client side: `client_fields.rs`, `client_route.rs`, `client_component.rs`. Read under tab 38 finding 3 A (one typed field tree from the manifest), tab 37 finding 1 A (typed contract files) and tab 35 finding 1 A (final route documents). Nothing built or run.

## Files read

- `src/client_route.rs` (non-test part, in full).
- `src/client_fields.rs`: `field`, `reference`, `text`, `values`, `input_fields_of` (head); function list.
- `src/client_component.rs`: header, `emit_ts_components`, `quote`; function list.

## Callers read

- `client_ir.rs` (`route_index` calls `client_route::evidence`; `build_operation` calls `input_fields_of`, `fields_of`, `schema_fields`).
- `materialize.rs` (`emit_ts_components`).

## One purpose per file

- `client_fields.rs`: build the field tree of a contract or of a JSON schema.
- `client_route.rs`: find a route's input schema and its response schema (from the component declaration of the route's operation).
- `client_component.rs`: write the wfe screen descriptions (TypeScript files of JSON descriptions checked against `@wfe/generator` types).

## Findings

**1. Input contracts come in two shapes.**

Fact: a custom operation's input contract carries a `fields` tree; a generated CRUD input contract carries `writable_fields`, `filters`, `id`, `sort`, `limit`, `pagination` and other members (`contracts.rs::input_contract`). `input_fields_of` reads the `fields` tree, and when there is none, walks the members and guesses which are inputs (it skips `pagination` by name because its `type` is not an input type). The route schema writer (`routes.rs`) reads both shapes too.

Options:

- A: one input contract shape for every operation: the typed field tree of tab 38 finding 3 A. Generated CRUD inputs are written as that tree. Delete the second shape, the guessing walk in `input_fields_of`, and its twin in `routes.rs`.

**2. Two matching declarations silently drop a route's response type.**

Fact: `client_route.rs::component_schema` returns `Ok(None)` when a second component declaration matches the same package, component and export. The route then has no response schema, and the client is generated without response types, with no error.

Options:

- A: refuse two matching declarations with an error naming both files.

## Follows from earlier rulings (no new ruling needed)

- Tab 38 finding 3 A and tab 45 finding 2 A: `fields_of` and `input_fields_of` stop parsing contract JSON; the typed field tree replaces them. `schema_fields` (field tree from a published JSON schema) stays for routes whose schema is authored.
- Tab 35 finding 1 A: `client_route` stops merging generated operations into each declaration (`route_schema::merge_operations`) and stops reading `.json.in` templates; it reads the one final declaration document.

## Fine as is

- `client_component.rs`: one description per screen as JSON, checked against the wfe types with `satisfies`; presentation only.

## Tests deleted, changed or moved

- Finding 1: tests of the CRUD member walk in `input_fields_of`, and CRUD input contract snapshots, change to the field tree.
- Finding 2: one test of the refusal.

## Not read

- `client_fields.rs` bodies of `insert`, `fields_of`, `schema_fields`, `object_fields`, `schema_field`; `client_component.rs` bodies after `emit_ts_components`; tests (names only).

## Rulings

- Findings 1–2: A (owner, 2026-11-11).

# 47 wamn-schema-generator, part 15

Scope: the TypeScript client emitter (`client_ts.rs`). Nothing built or run.

## Files read

- `src/client_ts.rs`: header, `ClientTsError`, `ts_type`, `to_camel`, `to_snake`, `RUNTIME_PACKAGE`, `RUNTIME_VERSION_RANGE`, `RESERVED_WORDS`, `emit_ts_package_json`, `shape_digest` (head), `write_result_field_map`, `write_field_map`; the function list of the rest.

## Callers read

- `materialize.rs` (the two `emit_ts_client` calls); `examples/check_client_ts.rs`, `examples/emit_client_ts.rs` (names).
- `web/runtime/src/wire.ts` (`FieldMap`, `fromWire`, `toWire`), `web/runtime/src/transport.ts::checkedMember`, `web/runtime/package.json`.

## One purpose per file

- `client_ts.rs`: write TypeScript bindings for one package: request and result interfaces, the published route, and one call function per operation, over the hand-written runtime `@wamn/web-runtime`.

## Findings

**1. A name translation layer between the wire and TypeScript.**

Fact:

- The wire and the contracts use snake\_case. The bindings give every member a camelCase name (`to_camel`), checked to reverse exactly (`to_snake`, `check_reversible`, `IrreversibleName`) and not to collide (`NameCollision`).
- To translate, every operation emits field maps as code (`write_field_map`, `write_result_field_map`, `write_map_members`), and the runtime walks every request and response to rename keys (`web/runtime/src/wire.ts::fromWire`, `toWire`). Error pointers are mapped back to members (`transport.ts::checkedMember`).

Options:

- A: TypeScript uses the wire names. Delete `to_camel`, `to_snake`, the reversibility and collision checks and their error types, the emitted field maps, `fromWire`, `toWire` and the rename in `checkedMember`. One name for a field everywhere: wire, contract, logs, errors, TypeScript.
- B: keep camelCase members (TypeScript convention) and the translation layer.
- Recommend A.

**2. The runtime version is written by hand.**

Fact: `RUNTIME_VERSION_RANGE = "^0.1.0"` restates the version in `web/runtime/package.json` (`0.1.0`). `emit_ts_package_json` builds the JSON by string formatting.

Options:

- A: read the runtime's version from `web/runtime/package.json` at build (`include_str!` and parse); build the package JSON with `serde_json::json!`.

## Follows from earlier rulings (no new ruling needed)

- Tab 39 finding 5 A and tab 45 finding 2 A: `ts_type` parses the type name string back into `ColumnType` through serde; the IR holds `ColumnType` and the one type table gives the TypeScript spelling.
- Tab 45 finding 2 A: string matches on operation type and result class (`PRIVATE_KIND`, `"bounded_list"`, `"page"`) become enums.

## Checked, no change

- TypeScript is written as text. Rust has no `quote!` for TypeScript; AST code generators (swc, oxc) are large for this. Kept.
- `RESERVED_WORDS`: needed while function names come from contract names.

## Tests deleted, changed or moved

- Finding 1: name rule tests (`to_camel`, `to_snake`, reversibility, collisions), field map snapshots, `web/runtime` tests of `fromWire`/`toWire`/`checkedMember` renames go or change; binding snapshots change to wire names.
- Finding 2: none beyond the package JSON snapshot.

## Not read

- `client_ts.rs` bodies of `emit_model`, `write_sync`, `emit_operation`, `write_changes`, `write_result`, `write_interface` (structure only); tests (names only).

## Rulings

- Finding 2: A (owner, 2026-11-11).
- Finding 1: B, keep (owner, 2026-11-11). TypeScript types stay camelCase; the generator decides every member name with the hand-written rule (\`to\_camel\`, checked by \`check\_reversible\`, which also proves no two names collide); the runtime applies the emitted field maps. No library. Prior art: this is the generated-converter pattern of \`openapi-generator\` (typescript-fetch \`FromJSON\`/\`ToJSON\`) and \`ts-proto\` (\`fromJSON\`/\`toJSON\`), with maps as data instead of a function per type. Alternatives recorded: wire names in TypeScript; a camelCase public wire (protobuf JSON style). Fix added: \`web/runtime/src/wire.ts::fromWire\` looks keys up with \`map\[name\]\` on a plain object, so an undeclared wire key named like an Object property (\`constructor\`, a valid snake\_case name) reads the built-in; look up own keys only (\`Object.hasOwn\`), with one test of an undeclared \`constructor\` key.

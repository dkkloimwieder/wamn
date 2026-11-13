# 38 wamn-schema-generator, part 7

Scope: the WIT writer and the JSON codecs it generates. Read under tab 37 rulings (typed contracts; `quote!`; take utilities from existing crates). Nothing built or run.

## Files read

- `src/generate/wit.rs`: `emit_model_wit`, `emit_model_package_wit`, `emit_crud_interface`, `emit_query_page`, `emit_operation_errors`, `emit_crud_result`, `emit_update_interface`, `access_error_literal`, `emit_crud_codec`, `emit_update_codec` (head), `emit_handler`, `emit_codec_support`, `codec_prelude`, the constants `CODEC_HEADER`, `CODEC_ENVELOPE`, `CODEC_CHANGE`, `QUERY_RUN`, `QUERY_EXPORT`, `UPDATE_CODEC_*`; `emit_custom_operation_wit`, `emit_custom_group`, `emit_forwarding_interface`, `emit_owned_interface` (head).
- `src/generate/wit_adapters.rs` (head: `emit_row_adapter`).

## Callers read

- `platform/component-composer/src/dispatch.rs` (export choice); `platform/engine/src/operation/native_call.rs` (`NativeInput`); `platform/engine/src/component_admission.rs` (handler signature check); `engine/src/operation.rs` (the one `NativeInput` constructor).
- `apps/platform/data/access/src/{lib.rs, scalar.rs}` (`wamn-data-access`).
- Every hand-written `run_json` under `apps/` (6) and their typed `run` calls, including `client_acme_receiving` calling `base::run`.
- `generate/component.rs` (forwarding imports); `wamn_catalog::manifest::AccessOperationErrorLiteral`.
- The generator's `Cargo.toml` and every use of `wit_parser`.

## One purpose per file

- `generate/wit.rs`: write each package's WIT interfaces for its operations, and a Rust JSON codec per operation.
- `generate/wit_adapters.rs`: write the row, error and export adapter macros that the codecs use.

## Findings

**1. Fixed codec code is kept in string constants and copied into every package.**

Fact:

- `CODEC_HEADER`, `CODEC_ENVELOPE` and `CODEC_CHANGE` are fixed Rust, written into each package as `generated/wit/operation_codec.rs`. `QUERY_RUN`, `QUERY_EXPORT`, the `UPDATE_CODEC_*` constants, `emit_handler`, `codec_prelude` and the `wit_adapters.rs` macros are fixed Rust with only type names filled in.
- This code has its own `canonical_uuid`. `wamn-data-access::scalar::uuid` already parses the one UUID spelling. `apps/wamn_receiving/.../record_receipt.rs::canonical_uuid` is a third copy (apps tab).
- `wamn-data-access` is the guest crate that generated data functions call. Its own doc says it exists so that packages stop copying this kind of code.

Options:

- A: move the fixed parts into `wamn-data-access`, compiled and tested once: envelope decode, count check, `request_id` check, `JsonChange`, int64 as a JSON string, the request loop, `CodecError`, and the export adapter (generic over the request and outcome types). Use `scalar::uuid`. Generated code keeps only the per-operation types and field lists. Delete the string constants.

**2. Every operation has two entry points, and the JSON one is written by hand in apps.**

Fact:

- Each operation interface exports `run` (typed WIT) and `run-json` (a JSON string), and reads also export `read-json`.
- The host and the composed dispatch call only `run-json`: `dispatch.rs` and `native_call.rs` take `run-json` first and `run` only when it is absent. Components call each other through typed `run` (for example `client_acme_receiving` calls `base::run`).
- `run-json` is always: decode JSON, call typed `run`, encode JSON. Generated CRUD gets it from the `export_operation!` macro; apps write it by hand, 6 times (`modbus_reader`, `netsuite`, `rest_connector` twice, `client_acme_receiving`, `wamn_integration`).
- `native_call.rs::NativeInput::Typed` has no constructor in product code (`operation.rs` builds only `NativeInput::Json`).

Options:

- Fact on the dispatch: `dispatch.rs` writes a WAT selector at compose time, with no compiler. It exports one `run(operation, json)` and passes the string to the matching `run-json`; it decodes nothing.
- The question is where JSON becomes typed values:
  - A (decode in each operation component): each operation exports `run-json`; one generic adapter from the support crate (finding 1), used for every operation; no hand-written `run_json`. Smallest change; the codec is compiled with the package. Every host-facing entry stays JSON for good; streams, resources and raw bytes cannot reach an operation from the host or HTTP.
  - B (decode in the dispatch guest): operations export only typed `run`; the host sends JSON to `dispatch.run(operation, json)`, which decodes and calls typed `run` inside the composition. Operations are purely typed and the host has no converter. The dispatch then needs real decode code per application, so composition must compile code (a WAT decoder is not practical; inference).
  - C (decode in the host): operations export only typed `run`; the host converts JSON to WIT values from the component's export types (wasmtime values); `NativeInput::Typed` becomes the only host path. WIT is the one contract; the host can pass a stream, a resource or raw bytes to typed `run`. Cost: one generic JSON-to-WIT converter in the host, which then owns the JSON rules (int64 as a string, absent vs null, UUID spelling).
- JSON overhead is the same at the HTTP edge in all three, and on workflow hops while `emission.payload` stays JSON.

**3. The WIT writer reads its own output back and depends on client types.**

Fact:

- `emit_model_wit` reads the `.result.json` it wrote a moment before from the in-memory files and parses it again (`expect("model result contract is emitted first")`).
- Four custom-operation emitters turn the typed manifest input into JSON (`serde_json::to_value(&operation.input)`) and parse it with `client_fields::input_fields_of` into `client_ir::FieldIr`.

Options:

- A: the emitters take the typed contract values (tab 37 finding 1) and one typed input field tree, built once from the manifest in `wamn-catalog`. The WIT writer, the client IR and the input contract all use that tree. Delete the read-back and the JSON round trips.

**4. WIT is written as text and patched by text search, and `wit-parser` is unused.**

Fact:

- `emit_custom_operation_wit` checks whether a group is written by searching the package file for `interface <name> {`, then appends by trimming the file and splitting on a blank line.
- `emit_custom_group` calls `emit_owned_interface`, which writes a stub interface for every sibling operation; the caller keeps only the text after `rfind(marker)`.
- `emit_query_page` builds a fixed text and patches it with `.replace("id: string", …)`.
- `wit-parser` is a dependency of the generator; nothing in `src/` or `tests/` uses it.

Options:

- A: build WIT with `wit-encoder` (wasm-tools), one package per group, built once; check the output with `wit-parser`. Same pattern as tab 37 finding 5 A. Delete the text search and patching.
- B: keep text output, but build each group once with no search or patch; drop the unused `wit-parser` dependency.
- Recommend A.

**5. Update has its own copy of the interface writer.**

Fact: `emit_crud_interface` hands update to `emit_update_interface` (called the "compatibility emitter" in its `unreachable!`). That function repeats the error-variant loop of `emit_operation_errors` and builds the result row from every table column; the other actions take the result row from the result contract. The update codec (`emit_update_codec`, `UPDATE_CODEC_*`) is also separate.

Options:

- A: one interface writer and one codec writer for all actions. Update's nested `change` record is one branch, and its result row comes from the result contract.

**6. Error names are spelled twice.**

Fact: `wit.rs::access_error_literal` spells the 13 names of `AccessOperationErrorLiteral` by hand; the enum already has them through `#[serde(rename_all = "snake_case")]`.

Options:

- A: one `as_str` on the enum in `wamn-catalog`, next to its serde names. Delete `access_error_literal`.

## Carried

- `apps/wamn_receiving/.../record_receipt.rs::canonical_uuid` copies `wamn-data-access::scalar::uuid`. Apps tab.

## Tests deleted, changed or moved

- Finding 1: the support code gets unit tests in `wamn-data-access`. Generated codec snapshot tests in `tests/generation.rs` and `tests/platform_generation.rs` change.
- Finding 2: tests of `NativeInput::Typed` in the engine are deleted. App tests keep testing through `run-json`.
- Findings 3–6: generator snapshot tests change text. `wit.rs` has no unit tests.

## Not read

- `wit.rs` custom codec bodies (`emit_custom_codec` to the end) and the rest of `wit_adapters.rs`, beyond their structure.
- `tests/support/platform_wit.rs`, `platform_protocol.rs`.

## Rulings

- Finding 2: C (owner, 2026-11-11). Operations export only typed `run`; the host converts JSON by the export type. Delete `run-json`, `read-json`, every generated and hand-written codec and `run_json`, the `run-json` paths in `dispatch.rs` and `native_call.rs` (`NativeInput::Json`), and the admission check of `run-json`. A and B are kept above as the alternatives considered.
  - Effect on finding 1: the fixed codec code is deleted, not moved; its JSON rules move into the host converter. The `scalar::uuid` reuse still applies there.
  - Open check: wasmtime's runtime value API covers every WIT type the operations use, at acceptable cost (including where `read-json` returned the WAL position).
- Re-entry, with tab 31 finding 3 A: owner chose 1 (2026-11-11).
  - The problem. Fact: a component instance cannot be entered while it waits on one of its own imports; wasmtime traps ("cannot enter component instance") and its maintainers say the spec requires it. Under C the route-table guest hands JSON to the host (`wamn:operation`) and the host calls typed `run`. If the route guest and the operations are one composed instance, that call re-enters it (inference; not checked whether the check also covers sibling instances in one composition).
  - Ruling: 1, the route table is its own component. The host runs two components per application: the route component and the application component. Route component → host (`wamn:operation`, JSON) → host converts → the application's typed `run`. No re-entry, as wasmCloud links components through the host. The composer no longer composes the route guest with the operations.
  - Alternatives considered:
    - 2: decide in the guest, act in the host. The route guest exports `route(request) -> decision`; the host calls typed `run`; a second guest call shapes the response. Keeps one composition; splits routing over two guest calls and needs guest state between them.
    - 3: same composition with async. The host runs the operation as a new top-level task and answers the pending import through a channel (pattern suggested by a wasmtime maintainer). Unknown whether the spec's recursion check still flags it in one composition.
    - 4: router in the host. Undoes tab 31 finding 3 A.
- Findings 1, 3–6: A (owner, 2026-11-11). Finding 1 as reduced by finding 2 C: the codecs go, and the host converter uses wamn-data-access::scalar::uuid. Finding 4 A: wit-encoder, checked with wit-parser. Finding 5 A: one interface writer (the codec half is gone under C).

## Check results (2026-11-11)

- **wasmtime value API.** Fact (wasmtime 48 docs): `component::Val` has 27 variants and covers every WIT type, including `Resource`, `Future`, `Stream` and `ErrorContext`. The docs call it "not an efficient representation". Inference: it replaces a JSON string copy into the guest plus a guest parse, so the cost is of the same order. This goes on the tab 34 benchmark list.
- **WAL position.** Fact: the host already reads it (`wamn_postgres/run_stream.rs` reads it beside the query) and passes it to the guest, which passed it back through `read-json`. Resolved: the host keeps it on the open read. Delete `want-wal-position`, the `wal-position` member of the row set, and the guest `wal_position` state.
- Correction recorded in tab 22: its finding 5 (delete the typed branch of `native_call.rs`) is reversed by finding 2 C.

Owner note (2026-11-11): the cost of the `Val` path must be benchmarked; the "similar cost" claim is an inference. Added to the tab 34 future work list.

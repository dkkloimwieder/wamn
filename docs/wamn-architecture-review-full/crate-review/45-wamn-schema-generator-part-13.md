# 45 wamn-schema-generator, part 13

Scope: the client contract IR (`client_ir.rs`), the first of the client files. Read under tab 37 finding 1 A (typed contract files) and tab 35 finding 1 A (the build writes final route documents once). Nothing built or run.

## Files read

- `src/client_ir.rs`: header, the types (`ClientContractIr`, `ModelIr`, `RouteIr`, `ResponseIr`, `FieldIr`, `OperationIr`, `RecordIr`, ...), `canonical_bytes`, `from_release`, `from_contract_directory`, `project`, `from_release_contracts`, `assemble`, `default_field_labels`, `published_routes`, `build_operation`, `bind_served_contracts` (head), `errors_of`; function list of the rest.

## Callers read

- Every caller of `from_release`, `from_contract_directory`, `from_release_contracts`, `canonical_bytes` and `CLIENT_IR_FORMAT_VERSION` (grep).
- `materialize.rs::client_bindings` and the host-route client path (the two product callers).
- Heads of `client_plan.rs`, `client_ts.rs`, `client_component.rs`, `client_route.rs` (what reads the IR).

## One purpose per file

- `client_ir.rs`: join one package's operation contracts with the routes its release publishes, into one in-memory model that every client emitter reads.

## Findings

**1. Test-only entry points and a serialized form nothing reads.**

Fact:

- `from_release` and `from_contract_directory` (with `project`, which reads the contract directory from disk and checks build freshness) have only test callers. Product code calls `from_release_contracts` only (`materialize.rs`, twice).
- The IR derives `Serialize` and `Deserialize`, carries `CLIENT_IR_FORMAT_VERSION`, and has `canonical_bytes`. No product code writes or reads a serialized IR; only tests compare its bytes (`tests/support/platform_client_ir.rs`). `client_plan.rs` says of its own plan: "generation does not serialize it, and it carries no version".

Options:

- A: the IR is an in-process value. Delete `from_release`, `from_contract_directory`, `project`, `canonical_bytes`, `CLIENT_IR_FORMAT_VERSION`, the serde derives and attributes, and the byte-stability tests.

**2. The IR reads the contracts as untyped JSON.**

Fact: `from_release_contracts` and `build_operation` look members up by string key in `serde_json::Value` (`member("grant")`, `.get("lists")`, `paging_of`, `errors_of`, `relations_of`, `string_list`, `split_contract_name`, `collect_module`, `read_json`). Tab 37 finding 1 A gives the contract files one typed shape in `wamn-catalog`.

Options:

- A: the IR builds from the typed contract values. It keeps only what it adds: the join with routes, the served-contract binding (`bind_served_contracts`), the field tree and default labels. Delete the string-key parsing and its error cases (`MalformedContract`, `MissingMember`).
- B: no IR; each emitter reads the typed contracts and the routes itself. The join (`bind_served_contracts`, labels) would then be repeated per emitter.
- Recommend A.

## Follows from earlier rulings (no new ruling needed)

- Tab 35 finding 1 A: `route_index` reads the one final route document; its merge of authored and generated documents and `$ref` resolution go.
- Tab 42 finding 1: the freshness check in `project` goes (with `project`, finding 1).
- Tab 37 finding 2 A: `OperationIr.grant` and `permission_token` go.

## Tests deleted, changed or moved

- Finding 1: `tests/support/platform_client_ir.rs` byte-stability and reorder tests go (they test the serialized form only); IR tests call `from_release_contracts`.
- Finding 2: tests that hand-write contract JSON for the IR build typed contracts.

## Not read

- `client_ir.rs` bodies of `route_index`, `build_model`, `paging_of`, `relations_of` (structure only).
- `client_plan.rs`, `client_ts.rs`, `client_component.rs`, `client_fields.rs`, `client_route.rs` (next parts).

## Rulings

- Finding 2: A (owner, 2026-11-11).
- Finding 1: A (owner, 2026-11-11).

# 37 wamn-schema-generator, part 6

Scope: the operation contract files and the generated Rust. Read under tab 34 (U2) and tab 35 rulings. Nothing built or run.

## Files read

- `src/generate/contracts.rs` (non-test part): `emit_model`, `emit_operation_contracts`, `emit_custom_operation_contracts`, `input_contract`, `error_contract`, `idempotency_contract`, `required_schema_contract`.
- `src/generate/rust.rs` (non-test part): `wamn_api`, `mutation_constraint_names`, `emit_projection`, `emit_rust_row`, `emit_wamn_accessor`, `emit_argument_count_expectation`.

## Callers read

- Every reader of `generated/contracts/`: `client_ir.rs::from_release_contracts` and its operation reader; `materialize.rs::client_bindings`; `control/package/src/component_admission.rs::{GeneratedOperationContract, load_package_statements}` and `component_admission/plan.rs`; `control/release/src/publish_release/package_sources.rs::{read_operation_contracts, read_input_identity}`; `client_plan.rs::idempotency_field`.
- `control/package/src/package_artifact.rs::PACKAGE_TAR_LAYERS`.
- Every reader of `required-schema-contract.json`.
- `generate/data.rs` (the `include!` of the generated Rust; the constraint constants).
- `web/runtime/src/transport.ts` (its use of error `sources` and `transaction`).
- `catalog/model/src/host_route/contract_files.rs` (head).

## One purpose per file

- `generate/contracts.rs`: write the four contract files of each operation (operation, input, result, errors), the cursor contract and the required schema contract.
- `generate/rust.rs`: build the typed accessor list of a model (`WamnApi`) and write it as Rust source.

## Findings

**1. The contract files have no type and no single reader.**

Fact:

- The generator builds each file by hand with `serde_json::Map` and `json!`.
- Three readers parse it, each its own way: `component_admission.rs::GeneratedOperationContract` (a private struct, plus `reject_retired_sql_files` to refuse a retired member), `package_sources.rs::read_operation_contracts::Contract` (another private struct), and `client_ir.rs` (untyped `Value` lookups by string key).
- The file layout `generated/contracts/{module}/{action}.{part}.json` is spelled in the generator (twice), in `component_admission/plan.rs` (`format!`), in `package_sources.rs` (suffix match, and string slicing to find the `.input.json`) and in `client_ir.rs` (`split_contract_name`).

Options:

- A: one serde type per contract file (operation, input, result, errors) and one function that reads a package's contracts, in the crate that owns the manifest types (`wamn-catalog`). The generator serializes the types; every reader calls the one function. Delete the private structs, `reject_retired_sql_files`, the `Value` lookups and the path spellings.

**2. Many contract members have no reader.**

Fact (grep of product code, tests excluded):

- Operation file: `automatic_retry`, `dependency` and `connection` have no reader. `idempotency` (the write-log description from `idempotency_contract`) is read only by tests.
- `grant` and `permission_token` are read into `client_ir::OperationIr`; no emitter reads those two fields (inference from grep of `client_*.rs`). `grant` is the operation id when public, else null, so it restates `operation` and `visibility`.
- Input file: `validation_order`, `invalid_cursor`, `invalid_limit` and `server_owned_fields.if_supplied` have no reader.

Options:

- A: delete these members, `idempotency_contract`, and `OperationIr.grant` and `.permission_token`. A member comes back only with a reader. (The idempotency key field itself: finding 3.)

**3. The idempotency key field is found three ways.**

Fact:

- The manifest has it (`CustomOperationDeclaration::idempotency_key_field`; `CREATE_KEY_FIELD` for a create).
- The operation file states it (`idempotency.key`); only tests read it.
- Release publish finds it again by scanning the input fields for a path in `client_plan::SUPPLIED_PATHS` (`client_plan::idempotency_field`, called from `package_sources.rs::read_input_identity` and `client_plan::route_canonicalization`).

Options:

- A: the operation file states the key field once (`idempotency_key`), taken from the manifest. Release and clients read that member. Delete `client_plan::idempotency_field` and its scan. (Whether `SUPPLIED_PATHS` has other uses: client tab.)

**4. The required schema contract has no product reader.**

Fact: `required_schema_contract` writes `generated/required-schema-contract.json`, and `PACKAGE_TAR_LAYERS` ships it in every package. Its only readers are tests: `apps/client_acme_receiving/tests/overlay_compatibility.rs` (with `overlay_compatibility_fixture.rs`, which checks an installed database against it in test code) and `package_artifact.rs::an_unpacked_artifact_packs_to_its_own_digest`.

Options:

- A: delete `required_schema_contract`, `REQUIRED_SCHEMA_CONTRACT_PATH`, the `RequiredSchemaContract` types and the tar layer entry, with their tests.

**5. Generated Rust is written as text, and the generator copies Clippy's limit.**

Fact:

- `rust.rs` (1,058 lines) and `data.rs` write Rust source with `writeln!` and `format!` strings. The workspace has no `quote`, `syn` or `prettyplease`.
- `CLIPPY_ARGUMENT_THRESHOLD`, `accessor_argument_count` and `emit_argument_count_expectation` copy Clippy's default argument limit, so that generated accessors carry an exact `#[expect(clippy::too_many_arguments)]`. If Clippy's default changes, generated code fails the lint.

Options:

- A: build generated Rust with `quote!` and format it with `prettyplease` (the standard Rust code generation tools). Mark the generated module once with `#[allow(clippy::all)]` at its `include!`. Delete the string writers and the Clippy copy.
- B: keep text output. Replace only the Clippy copy with the one module-level allow.
- Recommend A: it uses the existing tools, and the generator stops formatting Rust by hand. The same question comes up for `data.rs` and `component.rs` (Rust) and `wit.rs` (WIT; `wit-encoder` from wasm-tools) in their parts.

## Follows from tab 34 (no new ruling needed)

- Deleted with the native version: `Projection`, `ProjectionContents::Native`, `native_bind_fixtures`, `static_sql_native_bind_fixtures`, `native_inert_value`, `sql_constant_name`, `emit_native_bind_fixture`, the native branch of `emit_rust_row`, and the `native-verifier` and `source-map` outputs of `emit_model`.
- U2: accessor and statement types come from describe. Today the CRUD binds and rows come from the catalog (`wamn_api`, `bind_for_column`, `operation_result_row`, `contracts.rs::operation_row_columns`, `statement_value_contract`). These take describe's answer instead.

## Carried

- Host routes have 160 hand-written contract files under `catalog/model/src/host_route/`, in the generated format, with their own sharing scheme (`shapes.json`, `contract_files.rs`). This is a second producer of the format. Catalog tab; see also tab 31 (host routes become native ingress routes).
- The web client re-checks error cases against the contract (`transport.ts`: `sources` against `KNOWN_SOURCES`; `transaction`). Web tab.

## Tests deleted, changed or moved

- Finding 1: `component_admission` tests that write contract JSON by hand change to the typed contract; delete any test of the `sql_files` refusal. `client_ir.rs` tests build typed contracts.
- Finding 2: delete `tests/generation.rs::generated_create_contracts_publish_the_write_log_and_its_refusal` (the `idempotency` part) and the `idempotency` assertions in `tests/platform_generation.rs::a_claim_command_names_its_key_and_its_codec_claims_nothing`. Contract snapshot tests change.
- Finding 3: change `package_sources.rs::a_write_route_names_its_idempotency_field`; delete `client_plan` tests of `idempotency_field`.
- Finding 4: delete `tests/generation.rs::the_required_schema_contract_ignores_unused_tables`, `apps/client_acme_receiving/tests/overlay_compatibility.rs` and `overlay_compatibility_fixture.rs` (the required-contract parts); change the `package_artifact.rs` test that ships the file.
- Finding 5: generated Rust snapshot tests change text (prettyplease layout).

## Not read

- `client_ir.rs` beyond the contract reader; `client_plan.rs` beyond `idempotency_field` (client parts).
- `generate/data.rs`, `component.rs`, `wit.rs` bodies (next parts).
- `input_contract` body past the create and mutation branches.

## Rulings

- Findings 1–5: A. Finding 5 A: generated Rust is built with `quote!` and formatted with `prettyplease`; one `allow` on the generated module.
- Owner direction (2026-11-11): take needed utilities from existing crates where possible (single source of truth, single responsibility). Applies to every remaining part: before writing a helper, use the one in the crate that owns the concern, or an existing library.

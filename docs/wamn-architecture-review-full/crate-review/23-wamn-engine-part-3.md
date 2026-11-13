# 23 wamn-engine, part 3: engine build and component admission

Nothing was built or run. Every statement is from reading the code. **Fact** = read in the code. **Inference** = my conclusion.

## Files read

All under `crates/platform/engine/`.

| File | Lines | Non-test lines |
| --- | --- | --- |
| `src/component_admission.rs` | 1,322 | 590 |
| `src/engine.rs` | 1,088 | 281 |
| `src/statement_instances.rs` | 370 | 245 |
| `src/lifecycle.rs` | 209 | 78 |
| `src/warm_reuse.rs` | 87 | 61 |
| `src/declared_world.rs` | 81 | 81 |
| `tests/deadline.rs` | 57 | 0 |
| Total | 3,214 | 1,336 |

Callers read: `services/host/src/host.rs` and `services/edge/src/serve.rs` (engine build, warm reuse, lifecycle), `services/host/src/main.rs`, `src/integration.rs` (the two in-crate callers), `tests/integration` and `tests/conformance` (callers of `component_imports`), `crates/catalog/model/src/component_facts.rs` (the second import classifier), `crates/platform/component-policy/src/lib.rs` (registry rows), `wamn_package_artifact::sha256_digest`.

## One purpose each

| File | Purpose | Verdict |
| --- | --- | --- |
| `engine.rs` | Build the one Wasmtime engine | One purpose |
| `component_admission.rs` | Check component bytes and produce their facts | One purpose. Checks a typed path that is never called |
| `statement_instances.rs` | Check the statements wiring of a composed component | One purpose |
| `declared_world.rs` | Link only the declared WASI interfaces | One purpose |
| `warm_reuse.rs` | Which digests keep warm instances | One purpose |
| `lifecycle.rs` | Bounded shutdown and liveness watch | One purpose |

## Findings

### 1. The provider check guards a link that cannot exist (tab 22 finding 8, now decided)

- **Fact.** `declared_world.rs::linker` links only eight WASI interfaces, and only those the fact declares. The plugins then add their own interfaces. Nothing puts one component's export into another component's linker.
- **Fact.** Each component is its own workload.
- **Option.** Delete the "ambiguous component providers" check in `native_workload.rs` and its test. This is option A of tab 22 finding 8.

### 2. Admission checks the shape of a typed entry that the host never calls

- **Fact.** For an operation with a `run-json` adapter, admission also checks the typed `run`: two parameters, an owned list or record in, an owned list or record out. `owned_operation_value` walks the whole type for this.
- **Fact.** Its comment gives the reason: "Nested calls use separate stores. Only owned values can cross that boundary."
- **Fact.** The host calls `run-json`. The typed call path is dead (tab 22 finding 5, ruled for deletion).
- **Option A (recommended).** Keep the `run-json` type check and the rule that both are async. Delete `owned_operation_value` and the shape checks on the typed `run`.
- **Option B.** Keep the checks as a rule on guests for a later typed path.

### 3. Public parts that only tests use

- **Fact.** `validate_integration_admission` and `AdmittedIntegration` are called only by `integration/tests.rs`. Production calls `validate_connection_code`.
- **Fact.** `lib.rs::component_imports` is called only by `tests/integration` and `tests/conformance`.
- **Fact.** `ASYNC_HANDLER_OPERATION` is used only by tests of its own file.
- **Fact.** `build_engine_with_socket_policy` has one caller, inside the crate.
- **Option.** Delete `validate_integration_admission`, `AdmittedIntegration` and the constant; the tests call `validate_connection_code`. Put `component_imports` behind `test-util`. Make `build_engine_with_socket_policy` crate-private.

### 4. One mapping written three times

- **Fact.** "imports of the component type → instance type names" is written in `lib.rs::component_imports`, in `validate_connection_code` and in `validate_component_admission`.
- **Option.** One private function. The three call it.

### 5. The `wamn-node` world is generated twice in this crate

- **Fact.** `component_admission.rs` runs `bindgen!` on `wit/wamn-node` (`node_contract`). `operation.rs` runs it on the same world (`bindings`, exported as `node_types`).
- **Option.** `component_admission.rs` uses `crate::operation::node_types`. The second `bindgen!` goes.

### 6. The SHA-256 digest text is produced by several functions

- **Fact.** `component_admission.rs::component_digest` builds `sha256:<hex>` by hand with `sha2`.
- **Fact.** `wamn_package_artifact::sha256_digest` builds the same text with `ring` and `hex`.
- **Fact.** `runtime/src/connection_generation.rs::definition_hash` builds the same text again over JSON bytes.
- **Fact.** 37 non-test lines in `crates/` and `services/` call `Sha256::digest` directly. I counted them; I did not read each.
- **Correction to tab 16 finding 9.** That finding named a crate `wamn-digest`. No crate of that name exists in the repository. The ruling "one function" stands; its home was wrong.
- **Option.** One `sha256_digest(bytes)` in `wamn-execution-contract`, next to `canonical_json_sha256`. `component_digest`, the package-artifact function and `definition_hash` become calls to it. The 37 direct sites are checked crate by crate as each is read.

### 7. Two functions with one name and two rules

- **Fact.** `component_admission.rs::is_application_operation_import` says: platform when the package is registered, or when the namespace is `wasi` or `wamn`.
- **Fact.** `wamn_catalog`'s private function of the same name says: platform only when the package is registered.
- **Fact.** The engine comment explains the difference and names an incident that came from the two drifting.
- **Option.** One classifier in `wamn-component-policy` with three answers: registered platform package, unregistered `wasi` or `wamn` package, application package. Both callers use it and each picks the answers it needs.

### 8. Two engine builders differ by one optional path

- **Fact.** `build_engine_with_host_memory` and `build_engine_with_host_memory_and_compilation_cache` take the same arguments; the second adds the cache directory. `host.rs` picks one by whether the directory is set.
- **Fact.** The comment on `DEFAULT_CORE_INSTANCES` says workload admission keeps `pool_size` at zero. `warm_reuse.rs` sets a pool size for trusted digests.
- **Option.** One builder with `Option<&Path>` for the cache. Correct the comment.

## Carried from part 2, answered

- **Plugins are bound twice per component.** `load_component_workload` resolves the workload (wash-runtime binds the plugins), then replaces the linker with the declared-world linker. `load_native_application` then calls `on_workload_item_bind` again so the new linker has the plugin entries.
- **Inference.** The second bind is needed while wash-runtime gives no way to pass a linker into `resolve`. The repeated side effects (scope registration in each plugin) overwrite the same entry. I did not check `WamnPostgres::register_workload_scope` for this; it is checked in the `execution/host` tab.
- No option here: there is nothing in this crate to delete for it.

## Fine as is

- `engine.rs`: one inner builder; the pooling environment check; the abandoned-call grace set once, with the reason written down.
- `statement_instances.rs`: one pass, each refusal named.
- `declared_world.rs`: the eight interfaces cover the three WASI packages the policy registry admits (`wasi:io`, `wasi:clocks`, `wasi:random`).
- `warm_reuse.rs` and `lifecycle.rs`: small, one caller each.

## Tests

**Delete**

- `operation/native_workload/tests.rs::native_loader_refuses_ambiguous_imported_operation` (finding 1)
- `component_admission.rs::typed_async_operation_requires_owned_values`, `a_fixed_length_list_of_resources_is_not_an_owned_value` (finding 2 option A)

**Change**

- `integration/tests.rs`: calls of `validate_integration_admission` become `validate_connection_code` (finding 3)
- `component_admission.rs::an_async_lifted_run_on_async_handler_is_admitted`: the constant becomes its literal (finding 3)
- `engine.rs::a_serving_engine_persists_compiled_components_in_its_declared_cache`, `a_relative_compilation_cache_path_is_refused`, and the two outside callers of `build_engine_with_host_memory`: one builder (finding 8)
- `tests/integration/src/virtualized_std_guest.rs`, `tests/conformance/src/socket_test.rs`: need the `test-util` feature (finding 3)

**Move**

- `component_admission.rs::registered_capabilities_are_platform_imports_whatever_their_namespace`: to `wamn-component-policy` with the classifier (finding 7)

## Not read

- Test bodies: names only.
- `wamn-component-policy`: registry rows and function signatures only.
- `wamn_catalog::normalize_component_fact`: signature only.
- The 37 direct `Sha256::digest` sites.

## Rulings

None yet.

**Ruled 2026-11-11**

- Findings 1, 3 to 8: accepted. Finding 6 sets the home of the one digest function: `wamn-execution-contract`.
- Finding 2: **option A.** Delete `owned_operation_value` and the typed shape checks.
- Tab 23 is fully ruled. Tab 22 finding 8 is closed by finding 1 here.

**Correction, 2026-11-11, finding 6.** The home `wamn-execution-contract` conflicts with the tab 01 ruling: that crate stops existing, and the digest function goes to the new crate `wamn-digest`. The tab 01 ruling stands. The "one `sha256_digest`" part of finding 6 is unchanged.

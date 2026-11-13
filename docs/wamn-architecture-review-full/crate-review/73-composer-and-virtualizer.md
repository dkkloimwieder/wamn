# 73 wamn-component-composer and wamn-component-virtualizer

Scope: `crates/platform/component-composer` (`lib.rs` 828, `dispatch.rs` 411, `dispatch.wat` 30, `main.rs` 62) and `crates/platform/component-virtualizer` (`main.rs` 162) at `epic/dep7` (b94833fd). Nothing built or run.

## Earlier work checked first

- Boundary tab, owner ruling 2026-11-10: standard WASI imports go on the import list with an empty host context; guest bytes are no longer rewritten. The virtualizer, its two pins, the pin test, `tools/component-virtualization.json` and the rewrite branch in `generator/src/components.rs` go.
- Boundary tab: one application per workload is fine; component build and composition stay wamn functions.
- Tab 38 ruling 1: the route table is its own component; the composer no longer composes the route guest with the operations. Ruling C: the host converts JSON and calls the application's typed `run`.
- Tab 72 2 B: control routes become a native HTTP service.
- Tab 60: packages composed together are one ownership unit.

## Files read

- In full: `component-virtualizer/src/main.rs`, `component-composer/src/main.rs`, `Cargo.toml` of both.
- `component-composer/src/lib.rs`: header, public functions, `compose`, `compose_control_entry`, `validate_application_member`.
- `crates/platform/engine/src/statement_instances.rs` (header); `runtime/plugins/wamn_postgres/operation.rs::bind_operation_workload` (member statement sets).
- Upstream wasmCloud at tag `v2.11.0` (wamn pins `v2.12.1`): `wash-runtime/src/engine/workload.rs::link_components`, `engine/linked_call.rs` header.

## Callers read

- `crates/schema/generator/src/components.rs`: `tool`, `run_tool`, `virtualize`, `compose`.
- Repository grep for `wamn_component_composer::`: release `publish_release.rs`, package `component_admission.rs`, dev `dev/control.rs`, tests in release, dev, execution host and `tests/integration`; `Dockerfile` install and control entry step.

## One purpose per file

- `lib.rs`: compose an overlay with its bases; label each member's statements import; compose the route entry with an application; validate an application member.
- `dispatch.rs`, `dispatch.wat`: generate the selector that turns `run(operation, json)` into the matching `run-json` export.
- `main.rs` (composer): command line for overlay composition and the control entry.
- `component-virtualizer/main.rs`: rewrite a std guest's WASI imports with WASI-Virt.

## Findings

**1. The generator runs the composer and the virtualizer through `cargo run`.**

Fact: `components.rs::tool` starts `cargo run --locked --offline -p <tool>` from the repository root, writes a partial file and renames it (`run_tool`). The composer is also a library: release, package, dev and host code call `wamn_component_composer::` functions, and the generator's own tests call `compose` directly (dev-dependency).

Options:

- A: the generator calls `wamn_component_composer::compose` as a library function. `tool`, `run_tool`, the partial-file handling, the composer binary (`main.rs`) and its `Dockerfile` install go.

Recommend A.

**2. Overlays and bases are composed into one component, so statements need per-member labels.**

Fact:

- `compose` plugs each overlay import into the base that exports it and encodes one component.
- Because members then share one component, the composer gives each member its own import `sql-<position>` with an `(implements "wamn:postgres/statements")` label and a custom section `wamn:statement-instances` naming each member's digest (`name_statement_instances`, `label_statement_imports`, about 120 lines).
- `engine/statement_instances.rs` (370) verifies that record at admission; `wamn_postgres::bind_operation_workload` prepares one statement set per `sql-N`; the `wasm_component_model_implements` feature exists in `wamn-runtime`, `wamn-engine` and `wash-runtime` for these labels.
- Fact (upstream `v2.11.0`): `ResolvedWorkload::link_components` links one component's imports to another component's exports within a workload; `linked_call.rs` runs the callee in the caller's store or in a per-call store.

Options:

- A: keep build-time composition and the per-member labels.
- B: an overlay and its bases deploy as separate components of one workload; wasmCloud links them. Each component imports `wamn:postgres/statements` on its own. `compose`, the `sql-N` labels, the custom section, `statement_instances.rs`, the `implements` feature and the statement-instance fields in the catalog go. What is left of the composer, `validate_application_member`, moves to package admission.

Check result (2026-11-12): B does not keep one transaction.

- Fact (wamn): `wamn_postgres` keeps the open operation store-locally (`operation.rs::StoreOperation` in the calling context's resource table). `enter` rewrites that context's `component_id` to the operation scope. Statement sets (`statements.rs::active_statement_set`) and transactions (`operation_transaction.rs::operation_transaction_for`) are looked up by that `component_id`.
- Fact (upstream `v2.11.0`): every linked component gets its own `Ctx` built from its own template, with its own `component_id`. On the shared-store path, `new_store_from_templates` inserts one context per linked component (`shared_ctx.contexts`). On the ephemeral path, used when every parameter and result is a plain value (a typed `run` call), each call builds a new store and new contexts (`new_ephemeral_store`).
- Inference: a base's statement call looks up its own `component_id`, finds no active operation, and is refused; it cannot join the overlay's transaction on either path. Making B work needs the plugin to carry the operation scope across linked contexts; I found no hook for that in the upstream code read.
- Not read: the pinned `v2.12.1` source.

Recommend A: keep build-time composition and the per-member statement labels.

## Follows from earlier rulings

- Boundary ruling 2026-11-10: `wamn-component-virtualizer` (whole crate), the `wasi-virt` git dependency, `tools/component-virtualization.json`, `components.rs::virtualize`.
- Tab 38 rulings 1 and C: `compose_application`, `dispatch.rs`, `dispatch.wat`, `validate_application_dispatch`, `APPLICATION_DISPATCH`, `wit/wamn-application-dispatch` and the dispatch check in `validate_application_member` go.
- Tab 72 2 B: `compose_control_entry`, the route pin it reads, the composer's `--control-route` mode and the `Dockerfile` step that builds `control_http.wasm` go.

## Fine as is

- `validate_application_member`'s refusals of `wamn:operation` imports and of an application's own `wasi:http/handler` export.

## Tests deleted, changed or moved

- Follows: the virtualizer test; composer tests of `compose_application`, `compose_control_entry` and dispatch (13 tests in `lib.rs` and `dispatch.rs`, by count); `crates/control/release/tests/application_composition.rs` (route composition and control entry parts); `crates/control/dev/tests/edge_bundle.rs` (dispatch check); `tests/integration/src/control_client_live.rs` and `virtualized_std_guest.rs` (control entry).
- Finding 2 B: statement-instance tests in `engine/statement_instances.rs`, `execution/host/src/operation/native_policy/tests.rs` (`statement-instances-unreleased`), and the `wamn:statement-instances` cases in `application_composition.rs`.

## Not read

- `lib.rs` bodies of `name_statement_instances`, `label_statement_imports`, `instantiate`; `dispatch.rs` body.
- The `wash-runtime` linking code at the pinned `v2.12.1`.

## Rulings

- 2026-11-12: finding 1 A. Finding 2: no ruling until the transaction check was done; after the check, finding 2 A (keep build-time composition and per-member statement labels).

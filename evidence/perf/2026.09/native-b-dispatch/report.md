# Native dispatch checkpoint

Issue: `wamn-0ct2.2`. Status: blocked at the exact component selection boundary.

Superseding owner decision, 2026-09-10: [plan B](../../../../docs/architecture/wamn_native_alignment_plan.md#b-replace-manual-guest-execution-including-its-duplicate-caches) requires unique providers only for interfaces imported by the admitted closure.
It replaces the per-dependency selection contract. This report retains the original failed experiment and its findings.

## Scope

This checkpoint tests public native dispatch before production replacement.
The fixtures use scalar WIT functions to isolate runtime mechanics.
They do not prove the complete WAMN node interface or application delivery.

The WAMN source baseline is `826037ff03702a35a649ab9d21a280f6f021c15a`.
The upstream source is unmodified wasmCloud 2.9.0 at `68ebece9c537f8bb4b5c9999f274ec68d60f35a9`.
The runtime uses Wasmtime 47.0.4.
The proof uses the production WAMN engine builder and public native workload APIs.

## Deadline boundary

The caller places one absolute deadline around the complete native dispatch future.
This deadline includes root initialization, linked initialization, and execution.
Four cases test root initialization loops, linked initialization loops, execution loops, and explicit cancellation.
Each case requires memory return and a successful finite call through the same bounded allocator.
The negative control uses only the native relative deadline.
It requires observed guest allocation after that deadline and termination by an external watchdog.

The first run with Tokio's default current-thread scheduler fails the execution deadline assertion.
The isolated diagnostic records 6.153198497 seconds before control returns.
Tokio polls 61 scheduled tasks before its timer driver, while native epoch yields make the guest task runnable again.
At about 100 milliseconds per epoch yield, this sequence explains the observed delay.
Root initialization runs inside the enclosing future, before native dispatch spawns the execution task.

The final checkpoint selects Tokio's public `event_interval(1)` setting.
It also bounds elapsed time for guest entry and explicit cancellation.
All seven cases pass with the original deadline and cleanup limits.
The setting changes timer polling, not the deadline or upstream code.
This checkpoint does not change the production Tokio runtimes.
B must preserve prompt timer polling in its production embedding before adoption.

The enclosing deadline is 200 milliseconds.
Each cleanup and reply wait has a two-second bound.
The positive cases give the native response timer 30 seconds, so that timer cannot satisfy the enclosing deadline proof.
Separate subprocesses contain nonterminating guests.

This checkpoint does not prove nested registered-operation authorization, caller propagation, or child deadline inheritance in the production router.
Those conditions remain in B's acceptance.

## Exact component selection

The production manifest parser accepts two distinct component digests that export the same registered operation.
A dependency selects one exact digest within that release.
Removing the selected component causes the parser to refuse the manifest, despite the remaining export.
The fixtures derive their digest fields from their actual bytes.
This proves the manifest boundary, not full component admission.

A public host plugin consumes the root export and installs a nested policy function through `on_workload_item_bind`.
The two-component case tests an allowed result, a denied result, and direct child initialization failure.
The child traps during initialization, so successful root execution distinguishes the host function from automatic child linking.

The three-component case installs that host function before native resolution.
Native resolution rejects the duplicated child export before invoking the host function.
Its error contains `cannot disambiguate the provider`.

The decisive native code is `engine/workload.rs` at the pinned revision.
`resolve_workload_imports` checks duplicate exported interfaces before it processes the existing linker entries.
`UnresolvedWorkload` exposes no public component mutation or import-to-digest selection before resolution.
Its public binding hooks cannot suppress that earlier ambiguity check.
The public `components` map belongs to the resolved workload, which this case cannot create.

The executed mismatch triggers B's loading stop condition:

> a required context, candidate or loading boundary cannot be represented through public APIs. Record the exact obstacle before expanding the adapter. Do not copy internals or preserve duplicate machinery merely to report native adoption.

B remains blocked and retains its deletion contract.
Its exit requires a supported public resolution path that preserves the existing exact component selection within one admitted release.
That path must honor the host policy function before the interface-only ambiguity check.
The existing WAMN lifecycle remains the sole production implementation while this boundary remains unresolved.
No private source copy, fork, additional admission restriction, or separate workload per node is introduced.

## Validation

The final [checkpoint-004 result](checkpoint-004/result.json) exits 0 with seven passed, zero failed, and zero ignored tests.
The [complete output](checkpoint-004/cargo.log) records two binding cases and five deadline cases.
The build and execution take 6.330317 seconds.
The source hashes remain unchanged during execution.

The first [build receipt](checkpoint-001/result.json) exits 101 before any test executes.
Its fixture digest formatting lacks the required trait, and its Wasmtime import is redundant.
The correction uses the existing hexadecimal encoder and removes that import.
The next [execution receipt](checkpoint-002/result.json) records six passes and one deadline failure.
The [isolated diagnostic](deadline-diagnostic-001/result.json) reproduces that failure with a measured 6.153198497-second delay.
The final run changes timer polling and strengthens cancellation assertions without increasing any limit.
Each receipt retains a hash-matched source patch over the recorded baseline.

The final test artifact SHA-256 values are:

| Artifact | SHA-256 |
|---|---|
| `native_dispatch_binding-bf07e76457be8a17` | `16677da3a499f9376f6b86cce4a8aac8d1ec64a5ec1fd2546c5f5424a825eff8` |
| `native_dispatch_deadline-52c0411de170d512` | `bfc44913e7be8ba167dcc07a3fb01fc164bcecd23563cb112c56baf997114e57` |

The [command](checkpoint-004/command.json), [source identities](checkpoint-004/source.json), and compiler versions identify the exact proof inputs.
The runtime library emits 16 existing warnings.
Focused [Clippy](clippy-002/result.json) exits 0 in 0.936666 seconds, with no warnings in either new test file.
Existing library and dependency warnings remain.
The first Clippy run finds six warnings in the new fixtures.
The correction removes needless lifetimes and borrowing, names the map type, and uses a checked integer conversion.
The final test run follows those source changes and still passes all seven cases.
The native-only deadline control and allowed/refused host cases supply the distinguishing controls.
No production mutation runs because this checkpoint changes no production execution path.
Formatting and the staged whitespace check pass for source, documentation, and source patches.
The whitespace check excludes only raw `cargo.log` captures, which preserve the tools' emitted whitespace.

No production execution code is removed because this checkpoint does not convert a production path.
The duplicate caches, manual store lifecycle, candidate path, and nested path remain B's deletion targets.
The ledger and build recipe identify the checkpoint and its limits.
Ledger row 4 and the execution model remain unchanged.
No performance-sensitive production substitution or benchmark runs in this checkpoint.
The 2.9 comparison required for a completed substitution remains outstanding.

## Source anchors

The [dependency hashes](upstream-source.json) identify the clean pinned source behind these boundaries:

| Boundary | Source |
|---|---|
| Native compilation cache reached by workload loading | `wash-runtime/src/engine/mod.rs:702`, `initialize_workload` and `load_component_bytes` |
| Duplicate interface refusal before host linker inspection | `wash-runtime/src/engine/workload.rs:1158` |
| Public host binding hook | `wash-runtime/src/plugin/mod.rs:537` |
| Existing host function retained during native linking | `wash-runtime/src/engine/workload.rs:1351` |
| Initialization before spawned fresh execution | `wash-runtime/src/engine/dispatch.rs:759` |
| Native epoch yield | `wash-runtime/src/engine/abandon.rs:684` |
| Default scheduler batch and public polling control | `tokio-1.53.1/src/runtime/scheduler/current_thread/mod.rs:840`, `runtime/builder.rs:1229` |
| Runtime-independent self-wake | `wasmtime-47.0.4/src/runtime/store.rs:2277` |
| Exact release dependency selection | `crates/catalog/model/src/serving_manifest.rs:490` at the recorded WAMN baseline |

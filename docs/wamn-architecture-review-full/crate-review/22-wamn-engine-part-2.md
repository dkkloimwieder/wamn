# 22 wamn-engine, part 2: delivery and the operation call

Nothing was built or run. Every statement is from reading the code. **Fact** = read in the code. **Inference** = my conclusion.

## Files read

All under `crates/platform/engine/src/`.

| File | Lines | Non-test lines |
| --- | --- | --- |
| `router_delivery.rs` | 745 | 497 |
| `operation/native_workload.rs` (+ `tests.rs`, 369) | 869 | 500 |
| `operation/native_call.rs` (+ `tests.rs`, 312) | 723 | 411 |
| `operation/intent.rs` | 653 | 465 |
| `operation.rs` | 193 | 193 |
| `operation/invocation_policy.rs` | 181 | 181 |
| Total | 3,364 | 2,247 |

Callers read: `crates/execution/host/src/router_delivery.rs`, `host_route.rs`, `operation.rs`, `route.rs`, `operation/signed_result.rs`, `native_policy.rs`; `crates/execution/workflow/src/router_driver.rs`; `services/edge/src/delivery.rs`, `operation.rs`, `policy.rs`, `application.rs`; `runtime/src/plugins/wamn_postgres/operation.rs`; the guest `apps/platform/ingress/http-route/src/entry.rs` (deadline use); `wit/wamn-router-delivery-0.4/package.wit`; every `apps/` use of the delivery source.

## One purpose each

| File | Purpose | Verdict |
| --- | --- | --- |
| `router_delivery.rs` | Delivery plugin and the checks every host shares | Carries a source and an error that nothing uses |
| `operation.rs` | One export call, shared by routes and wiring nodes | One purpose |
| `operation/intent.rs` | Write log rules of a logged call | One purpose |
| `operation/native_call.rs` | The native dispatch of one call | Carries a typed path and a second entry that production never uses |
| `operation/native_workload.rs` | Load an application as native workloads | One purpose |
| `operation/invocation_policy.rs` | The traits a host implements | One purpose |

## Findings: delivery

### 1. The registration source cannot succeed and nothing sends it

- **Fact.** `resolve_target` returns `None` for `SourceRef::Registration`, so `resolve_authorized_target` always answers `source-not-found` for it.
- **Fact.** No guest in `apps/` builds `source::registration`. The materializer admits events through `wamn:jetstream/registration`.
- **Fact.** `SourceRef::platform` has no caller outside tests.
- **Fact.** `parent-causation` is accepted only for a registration source (`execution/host/src/router_delivery.rs`). So no delivery can carry it.
- **Option.** Delete the `registration` case of `source` and `parent-causation` from `wamn:router-delivery`. Delete `SourceRef::Registration`, `SourceRef::platform`, the registration arm of `caller_matches_source`, and the parent half of `derived_causation`. The WIT version changes. The host and edge callers lose their registration arms.

### 2. `fresh-credential-required` is never produced

- **Fact.** No host code builds `DeliveryError::FreshCredentialRequired`. Only the guest reads it, and one test entry point maps it.
- **Option.** Delete the WIT case, the constant `FRESH_CREDENTIAL_REQUIRED` and the guest arm.

### 3. Small dead parts in `router_delivery.rs`

- **Fact.** `ResolvedTarget.caller_attached` is always `true` and has no reader.
- **Fact.** `authorize_attachment_for_test` has no caller.
- **Fact.** `OperationRefusalType` has one case, `PermissionDenied`.
- **Fact.** `authorize_released_operation` only forwards to `authorize_registered_operation`.
- **Option.** Delete the field, the test entry point, the one-case enum (the refusal keeps its operation name) and the forwarding function (three callers call the real one).

### 4. `operation-deadline-ms` (carried from tab 21 finding 3)

- **Fact.** The value is the host call ceiling, the same for every route.
- **Fact.** The guest needs it: `entry.rs::execute` sets one deadline for all request elements of a call from it. The guest has no other way to learn the host ceiling.
- **Option A (recommended).** Keep the field.
- **Option B.** Replace it with one host function that returns the ceiling. Same data, one more import.

## Findings: the operation call

### 5. The typed path is never used

- **Fact.** Nothing builds `NativeInput::Typed`. `operation.rs::call_export` always sends `NativeInput::Json`.
- **Fact.** So `NativeOutcome::Typed` never exists in production. The typed branch of `native_call.rs`, `context_value`, and `NativeOutcome::into_json` exist only for it.
- **Fact.** `execution/host/src/operation/signed_result.rs` has a typed branch for the same case.
- **Fact.** The comment says nested known calls keep WIT values. `native_workload.rs` says nothing links across applications.
- **Option.** Delete `NativeInput`, `NativeOutcome` (the result is the JSON result type), the typed branch, `context_value` and `into_json`. The typed branch of `signed_result.rs` goes in the `execution/host` tab.

### 6. Two of the four entry functions run only in tests

- **Fact.** `invoke_raw` is the only production entry (`call_export`). `invoke_owned` has no outside caller. `invoke_native` is called only by tests of `execution/host` (11 sites).
- **Fact.** They differ by one flag, `finish_result`. Production always passes `false`; the result is finished in `invoke_operation` after commit.
- **Option.** One `invoke` function without the flag. Delete `invoke_owned`, `invoke_native`, `finish_result` and the in-call finish branch. The 11 test sites call `invoke`.

### 7. The deadline and the application are worked out two or three times per call

- **Fact.** `invoke_operation`, `call_export` and `invoke_logged` each compute `now + deadline_ms`. The three instants differ slightly.
- **Fact.** `OperationCall.deadline_ms` repeats `context.deadline_ms`; its comment says it is the same value.
- **Fact.** Each of the three also calls `application_of` and `component_id_of` again.
- **Fact.** `component_id` finds the id by comparing whole `AdmittedComponent` values in a loop.
- **Option.** `invoke_operation` resolves the application, the component id and one deadline instant once, and passes them down. `OperationCall.deadline_ms` goes. `NativeWorkload` keeps a small index from the component's identity fields to its id.

### 8. A provider check for links that cannot exist

- **Fact.** `load_native_workload` refuses two components that export the same imported interface ("ambiguous component providers").
- **Fact.** The same file states that each component is its own workload and nothing links across them.
- **Inference.** The check guards a link that the loader no longer makes.
- **Option A.** Delete the check and its test `native_loader_refuses_ambiguous_imported_operation`.
- **Option B (recommended until part 3).** Decide after `declared_world.rs` is read, because it builds the linker that would show whether an import can reach a sibling.

### 9. The unbind on failure is written six times

- **Fact.** `native_workload.rs` repeats "unbind all plugins, add context, return the error" at six error sites.
- **Option.** One helper that takes the error. The comment that explains why a guard cannot do this stays.

### 10. A comment that the code no longer supports

- **Fact.** `load_component_workload` says "Plugins clear their state by workload id prefix." Tab 13 finding 8 showed the prefix cleanup removes nothing and ruled exact ids.
- **Option.** Correct the comment when tab 13 finding 8 is applied.

## Carried

- `load_native_application` calls `on_workload_item_bind` of each bound plugin a second time, because the linker made by wash-runtime is replaced by the declared-world linker. To part 3, with `declared_world.rs`.
- `IntentContext.tenant` is one more spelling for the carried "environment spelled five ways" list.
- `intent.rs` is the host side of the request element loop. The guest side is in `http-route/src/entry.rs`. Tab 12 finding 5 ruled what the two share. To the `http-route` tab.

## Fine as is

- `intent.rs`: claim, run each new request element alone, finish or release, merge in input order. Each branch has one answer.
- `invocation_policy.rs`: three traits, each with one implementer per host.
- `native_call.rs` scope handling: authority is revoked before the native identity is restored, also on cancel.
- `settle_route`: one place lowers a route result.

## Tests

**Delete**

- `router_delivery.rs::a_registration_delivery_executes_as_the_materializer_and_an_attachment_as_its_caller` (finding 1)
- `native_workload/tests.rs::native_loader_refuses_ambiguous_imported_operation` (finding 8, option A only)

**Change**

- `router_delivery.rs::source_ids_resolve_only_the_manifest_target_and_derive_caller_attachment`: drops the registration case and `caller_attached` (findings 1, 3)
- `router_delivery.rs::host_mints_current_causation_and_only_inherits_parent_root_depth`: keeps the case without a parent (finding 1)
- `router_delivery.rs::caller_handle_must_match_the_loaded_attachment_identity`: no registration arm (finding 1)
- `apps/platform/ingress/http-route/tests/adversarial.rs`: the `FreshCredentialRequired` case goes (finding 2)
- `crates/execution/workflow/src/queue/automation_live.rs`: two calls of `authorize_released_operation`, one use of `OperationRefusalType` (finding 3)
- `crates/execution/host/src/operation/native_policy/tests.rs` and `tests/warm.rs`: 11 `invoke_native` sites call `invoke` (finding 6)
- `crates/execution/host/src/operation/signed_result.rs` tests: two typed cases go with the typed branch (finding 5, in that tab)

**Move**

- None in this part.

## Not read

- Test bodies: names only.
- `crates/execution/host/src/operation/signed_result.rs`: the match on the outcome only.
- `wamn_run_state::IntentStore`: the trait signature only.
- wash-runtime `engine::dispatch`: signatures only.

## Rulings

None yet.

**Ruled 2026-11-11**

- Findings 1, 2, 3, 5, 6, 7, 9, 10: accepted.
- Finding 4: **option A.** `operation-deadline-ms` stays.
- Finding 8: **option B.** Decided in part 3, after `declared_world.rs` is read.
- Tab 22 is ruled; finding 8 is open until part 3.

Correction (2026-11-11, tab 38 finding 2 C): finding 5 deleted the typed branch of `native_call.rs`. Tab 38 reverses this: the typed `Val` branch (`NativeInput::Typed`) is kept as the only host path, and the JSON branch (`NativeInput::Json`) is deleted.

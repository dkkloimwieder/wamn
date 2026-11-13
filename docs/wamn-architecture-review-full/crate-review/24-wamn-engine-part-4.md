# 24 wamn-engine, part 4: integrations, release, artifacts (last part)

Nothing was built or run. Every statement is from reading the code. **Fact** = read in the code. **Inference** = my conclusion.

## Files read

All under `crates/platform/engine/src/`.

| File | Lines | Non-test lines |
| --- | --- | --- |
| `integration.rs` (+ `integration/tests.rs`, 604) | 1,360 | 675 |
| `release_manifest.rs` | 671 | 356 |
| `artifact_source.rs` | 430 | 262 |
| `expected_router.rs` | 337 | 112 |
| `component_artifact.rs` | 184 | 104 |
| Total | 2,982 | 1,509 |

Callers read: `runtime/src/integrations.rs` and `connection_health.rs` (integration load and imports), `execution/host/src/operation/native_policy.rs` (interfaces), `services/host/src/host.rs` (release load, local source, router), `services/edge/src/release.rs` and `serve.rs`, `runtime/src/component_artifact_source.rs` (layout and body checks), every use of `.release().manifest_digest`, every use of the component media types in the repository.

## One purpose each

| File | Purpose | Verdict |
| --- | --- | --- |
| `integration.rs` | Run an integration in its own store; route named imports to it | One purpose |
| `release_manifest.rs` | The one loaded release of a process | One purpose. Describes a carrier that is gone |
| `artifact_source.rs` | Trait and checks for component bytes; local file source | One purpose |
| `component_artifact.rs` | Layout of a component artifact | A wrapper around four constants |
| `expected_router.rs` | Answer "unavailable" for an expected host and during stop | One purpose |

## Findings

### 1. The mount carrier is gone, and the file header still describes it

- **Fact.** `LoadedRelease::load` (the standard mount path) has no caller. `RELEASE_MANIFEST_MOUNT_PATH` is used only by it.
- **Fact.** The header (70 lines) describes a ConfigMap carrier and names "reader 3: jetstream delivery gates on `ServingManifest::registrations`". The manifest has no `registrations` field.
- **Fact.** `load_from` is still used, by the local application path.
- **Option.** Delete `load` and the mount path constant. Replace the header with the current facts: bytes come from the registry source or a local directory; identity comes from the bytes; one instance per process.

### 2. `CarriedRelease` wraps one field

- **Fact.** `CarriedRelease` holds only `manifest_digest`. 41 call sites write `.release().manifest_digest`.
- **Option.** `LoadedRelease::manifest_digest()`. Delete the struct and `release()`. The 41 sites change by one name.

### 3. "The release carries this component" is checked by two functions

- **Fact.** `validate_component_in_release` compares package, component, interface version, digest and each operation. Its only production caller is the edge.
- **Fact.** `runtime/src/local_application.rs::validate_local_facts` makes the same comparison field by field.
- **Fact.** Tab 20 finding 4 moves the local formats next to this file.
- **Option.** One function "manifest carries fact". Both call it.
- **To check in the `execution/host` tab.** The comment says the cloud host runs this check too. I found no cloud caller.

### 4. `ComponentArtifactLayout` is a wrapper around four constants

- **Fact.** The struct holds two byte slices. Its two byte getters have no caller.
- **Fact.** The puller builds it with two empty slices only to read: schema version 2, layer count 1, and the two media types.
- **Fact.** The two media type constants have no user outside this file.
- **Option.** Delete the struct and `component_artifact_layout`. Keep four constants; the puller reads them.
- **To check in the `control/package` tab.** Where the publisher takes these values from. The header calls this file the shared contract of both sides, but I found only the puller using it.

### 5. A transferred body is checked by three functions

- **Fact.** `verify_component_body` and `verify_config_body` here, and `verify_transferred_body` in `runtime/src/release_manifest_source.rs`, each do: size equals the descriptor, then digest or bytes equal the expected value.
- **Fact.** `LocalComponentSource::pull_entry` does the digest half inline.
- **Option.** One function that returns which check failed. Each caller keeps its own refusal literal. It moves with the shared registry source of tab 20 finding 1.

### 6. Integration: parts that are public or kept without need

- **Fact.** `Integration::prepare` and `PreparedIntegration` are public. Outside callers use only `Integration::load`.
- **Fact.** `AddressEngine.admitted` keeps one compiled template per digest for the life of the process. Production only asks `contains_key`. Nothing removes an entry.
- **Inference.** The kept template holds the compiled code alive, so a new binding of the same digest does not compile again. Each new integration version adds an entry that stays.
- **Option.** `prepare` and `PreparedIntegration` become private. `admitted` becomes a bounded `moka` map by digest (the cache crate ruled in tab 17). An evicted digest compiles again on its next load.

### 7. Three engines in one host process

- **Fact.** Integrations run in two extra Wasmtime engines, one per address class (private allowed or not). Each is built with the default host memory budgets, the same as the main engine.
- **Fact.** The reason is in the code: the socket policy is set per engine.
- **Open fact.** What the two extra engines reserve. I cannot tell without a run.
- **Option A (recommended).** Keep. Measure before changing.
- **Option B.** Build the two address engines with a smaller core-instance budget. Needs a number from the owner.

## Fine as is

- `integration.rs`: exact authority only, metadata hosts refused, private ranges only by literal address, credential shown only for `custom` auth, one store per integration, each call under the host call ceiling.
- `artifact_source.rs`: one trait, two implementers (local here, registry in the runtime).
- `expected_router.rs`: every other method forwards to the native router. The cloud host passes no expected host; the wrapper then only adds the refusal during stop.
- `release_label`: one caller, the reason is written down.

## `wamn-engine` is read in full

- 4 parts, tabs 21 to 24.
- Open after this crate: the held scope record now covers `InvocationTraces` (tab 21 finding 7).

## Tests

**Delete**

- None in this part.

**Change**

- `component_artifact.rs::component_artifact_wire_literals_are_pinned`: reads the constants (finding 4)
- `runtime/src/component_artifact_source.rs::manifest_must_match_both_descriptors_and_exactly_one_layer`: reads the constants (finding 4)
- `release_manifest.rs::the_release_identity_comes_from_the_verified_content` and every test that writes `.release().manifest_digest`: one name (finding 2)

**Move**

- `artifact_source.rs::bodies_are_independently_verified_before_component_bytes_return` and the two body tests of `runtime/src/release_manifest_source.rs`: with the one body check (finding 5)
- `release_manifest.rs::an_admitted_component_the_release_does_not_carry_is_refused` stays; the local facts tests of tab 20 join it (finding 3)

## Not read

- Test bodies: names only.
- `runtime/src/integrations.rs`: the call sites only (the file was read in tab 16).
- `crates/control/package`: only the grep for the media types.
- wash-runtime `host::http::DynamicRouter`: signatures only.

## Rulings

None yet.

### 8. One instance per connection, one call at a time (added 2026-11-11, from the owner's question)

- **Fact.** `runtime/src/integrations.rs::resolve` keeps one `Integration` per (connection id, generation, credential).
- **Fact.** `Integration::call` locks that one store. Calls to one connection run one after another, each up to the 30 second host call ceiling.
- **Fact.** The integration is already loaded as a wash-runtime workload (`initialize_workload`, `resolve`). Only the last step is by hand: `new_store`, `instantiate_pre`, `call_async`, and the mutex.
- **Fact.** wash-runtime has a native path for this: `ResolvedWorkload::dispatch_target` and `DispatchTarget::dispatch`, with per-component `pool_size`, `max_concurrency`, `max_invocations` and reclaim settings. Applications already run this way (`operation/native_call.rs`).
- **Fact.** The socket policy stays per engine in wash-runtime, so the two integration engines of finding 7 stay.

**Owner direction 2026-11-11:** "ideally we are using B - we want to stay as close as possible to wasmcloud best practices."

- **Option (follows the direction).** Replace `IntegrationStore` (hand-made store, instance and mutex) and `PreparedIntegration::start` with a native dispatch target. The pool size comes from the component settings. An exclusive connection keeps pool size 1 and still takes its resource lease.
- **Open, to check before this is applied.**
  - Admission requires the health export to use the synchronous ABI. Whether a dispatched call can run a synchronous export is not checked.
  - Integration calls pass dynamic `Val` values. The typed branch of `native_call.rs` did this and is ruled for deletion (tab 22 finding 5). The integration call needs its own small `GuestCall`.
  - Whether each integration guest is safe as several instances against one remote system. That depends on the integration; the pool size must be settable per integration type.
- **Effect on finding 7.** The core-instance budget of the two integration engines then follows connections times pool size.

**Ruled 2026-11-11, finding 7:** keep the two shared engines (they are per address class, not per integration). Their budget is decided with finding 8.

**Ruled 2026-11-11**

- Findings 1 to 6: accepted.
- Finding 8: accepted as written. The three open checks are done before it is applied.
- Tab 24 is fully ruled.

**Correction, 2026-11-11, finding 3.** A cloud caller of `validate_component_in_release` exists: `crates/execution/host/src/operation.rs::validate_released_components`. The option is unchanged.

## Check results, finding 8 (2026-11-11)

- **Synchronous health export through a dispatched call.** Inference: yes. wash-runtime's pooled path calls any linked export with `call_concurrent` (`instance_driver.rs`) and does not check how it is lifted, and the component model lets an async caller call a sync export. Confirm at build.
- **Dynamic values.** Resolved by tab 38 finding 2 C: the host's typed `Val` path becomes the one host call path, and integrations use it. No separate `GuestCall` is needed beyond the one dispatch call.
- **Several instances against one remote system.** Resolved as a rule: `pool_size` is set per integration type, default 1 (today's behaviour). An integration type opts in to more.
